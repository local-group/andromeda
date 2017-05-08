
use std::fmt;
use std::cmp;
use std::time::{Instant, Duration};
use std::net::SocketAddr;
use std::collections::{HashMap};
use std::sync::mpsc::{Sender, Receiver};

use futures::sync::mpsc;
use futures::{Sink, Future};
use tokio_core::io::{EasyBuf};

use mqtt::{Encodable, Decodable, TopicName, QualityOfService};
use mqtt::packet::{
    QoSWithPacketIdentifier,
    Packet, VariablePacket,
    ConnectPacket, ConnackPacket,
    PingrespPacket,
    SubackPacket, UnsubackPacket,
    PublishPacket, PubackPacket, PubrecPacket, PubrelPacket, PubcompPacket
};
use mqtt::packet::suback::{SubscribeReturnCode};
use mqtt::control::{FixedHeader, ConnectReturnCode};

use super::{ClientSessionMsg, ClientConnectionMsg, LocalRouterMsg,
            SessionTimerAction, SessionTimerPayload, SessionTimerPacketType};
use store::{GlobalRetainMsg};


pub fn run(
    client_session_rx: Receiver<ClientSessionMsg>,
    session_timer_tx: Sender<(SessionTimerAction, SessionTimerPayload)>,
    client_connection_tx: mpsc::Sender<ClientConnectionMsg>,
    local_router_tx: Sender<LocalRouterMsg>,
    global_retain_tx: Sender<GlobalRetainMsg>,
) {
    let mut addrs = HashMap::<(u32, String), SocketAddr>::new();
    let mut sessions = HashMap::<SocketAddr, Session>::new();
    let mut persistents = HashMap::<(u32, String), PersistentSession>::new();
    // TODO: Read timeouts from config file
    let recv_packet_timeout = Duration::from_secs(20);
    let default_keep_alive = Duration::from_secs(30); // aws-iot: 5 => 1200 seconds

    loop {
        let msg = client_session_rx.recv().unwrap();
        // debug!("> Got message={:?}", msg);
        match msg {
            // Receive data from client_connection
            ClientSessionMsg::Data(addr, data) => {
                if !sessions.contains_key(&addr) {
                    sessions.insert(addr, Session::new(
                        addr,
                        recv_packet_timeout,
                        default_keep_alive,
                        session_timer_tx.clone(),
                        client_connection_tx.clone(),
                        local_router_tx.clone(),
                        global_retain_tx.clone(),
                    ));
                    // Set decode packet timeout
                    let timer_msg = (SessionTimerAction::Set(Instant::now() + default_keep_alive),
                                     SessionTimerPayload::KeepAliveTimer(addr));
                    session_timer_tx.send(timer_msg).unwrap();
                }
                let mut session = sessions.get_mut(&addr).unwrap();
                session.recv_packet(&mut addrs, &mut persistents, addr, data);
            }
            ClientSessionMsg::ClientDisconnect(addr, reason) => {
                // * Remove related information in `local_router`
                // * Send last will packet
                // * Close Client connection
                // * Remove current session
                if let Some(mut session) = sessions.remove(&addr) {
                    let msg = ClientConnectionMsg::DisconnectClient(addr, reason);
                    client_connection_tx.clone().send(msg).wait().unwrap();

                    if session.connected {
                        addrs.remove(&(session.user_id().unwrap(), session.client_identifier().unwrap()));
                        let mut clean_session = true;
                        if let Some(ref pkt) = session.connect_packet {
                            clean_session = pkt.clean_session();
                        }
                        if clean_session {
                            let msg = LocalRouterMsg::ClientDisconnect(
                                session.user_id().unwrap(), session.client_identifier().unwrap()
                            );
                            local_router_tx.clone().send(msg).unwrap();
                        }

                        // Handle will message
                        let mut publish_pkt: Option<PublishPacket> = None;
                        if let Some(ref connect_packet) = session.connect_packet {
                            if let Some((topic, payload)) = connect_packet.will() {
                                // * Send last will packet
                                let qos = match connect_packet.will_qos() {
                                    0 => QoSWithPacketIdentifier::Level0,
                                    1 => QoSWithPacketIdentifier::Level1(0),
                                    2 => QoSWithPacketIdentifier::Level2(0),
                                    _ => unreachable!()
                                };
                                let mut packet = PublishPacket::new(
                                    TopicName::new(topic.to_string()).unwrap(),
                                    qos, payload.clone()
                                );
                                packet.set_retain(connect_packet.will_retain());
                                publish_pkt = Some(packet);
                            }
                        }
                        if let Some(ref pkt) = publish_pkt {
                            // Boardcast the will message from this disconnected client.
                            session.recv_publish(pkt, true);
                        }

                        if !clean_session {
                            persistents.insert(
                                (session.user_id().unwrap(), session.client_identifier().unwrap()),
                                session.persistent.unwrap()
                            );
                        }
                    }
                }
            }
            // Receive packet from `local_router`
            ClientSessionMsg::Publish(user_id, client_identifier, subscribe_qos, mut packet) => {
                debug!("[ClientSessionMsg::Publish]: client_identifier={:?}, subscribe_qos={:?}, packet={:?}",
                       client_identifier, subscribe_qos, packet);
                // * Forward encoded packet to Client
                // * [if(qos > 0)] Save packet in current session.packets:
                //    > [server:PublishPacket]

                if let Some(addr) = addrs.get(&(user_id, client_identifier.clone())) {
                    // <Spec.RETAIN>:
                    // =============
                    // It MUST set the RETAIN flag to 0 when a PUBLISH Packet is
                    // sent to a Client because it matches an established
                    // subscription regardless of how the flag was set in the
                    // message it received [MQTT-3.3.1-9].
                    packet.set_retain(false);

                    // <Spec.DUP>:
                    // ==========
                    // The value of the DUP flag from an incoming PUBLISH packet is
                    // not propagated when the PUBLISH Packet is sent to subscribers
                    // by the Server. The DUP flag in the outgoing PUBLISH packet is
                    // set independently to the incoming PUBLISH packet, its value
                    // MUST be determined solely by whether the outgoing PUBLISH
                    // packet is a retransmission [MQTT-3.3.1-3].
                    packet.set_dup(false);

                    let qos_val = cmp::min(packet.qos_val(), subscribe_qos as u8);
                    if qos_val == 0 {
                        packet.set_qos(QoSWithPacketIdentifier::Level0);
                        encode_to_client(*addr, &packet, &client_connection_tx);
                    } else {
                        if let Some(mut session) = sessions.get_mut(&addr) {
                            let pkid = session.incr_pkid();
                            match qos_val {
                                1 => {
                                    packet.set_qos(QoSWithPacketIdentifier::Level1(pkid));
                                    session.send_publish(&packet, &client_connection_tx);
                                }
                                2 => {
                                    packet.set_qos(QoSWithPacketIdentifier::Level2(pkid));
                                    session.send_publish(&packet, &client_connection_tx);
                                }
                                _ => {
                                    error!("Invalid qos value: {:?}", qos_val);
                                }
                            }
                        } else if let Some(mut persistent) = persistents.get_mut(&(user_id, client_identifier)){
                            // TODO: 
                        } else {
                            // TODO: why can not find?
                            error!("[ClientSessionMsg::Publish]: Cant not find target session, addr={:?}", addr);
                        }
                    }
                } else {
                    warn!("[ClientSessionMsg::Publish]: Can find addr for >> user_id={}, client_identifier={}",
                          user_id, client_identifier);
                }
            }
            ClientSessionMsg::RetainPackets(_, addr, packets, subscribe_qos) => {
                let mut session = sessions.get_mut(&addr).unwrap();

                for mut packet in packets {

                    // <Spec.RETAIN>:
                    // =============
                    // When sending a PUBLISH Packet to a Client the Server MUST set
                    // the RETAIN flag to 1 if a message is sent as a result of a
                    // new subscription being made by a Client [MQTT-3.3.1-8].
                    packet.set_retain(true);

                    // See: [MQTT-3.3.1-3]
                    packet.set_dup(false);

                    let pkid = session.incr_pkid();
                    let qos_val = cmp::min(packet.qos_val(), subscribe_qos as u8);
                    match qos_val {
                        0 => {
                            packet.set_qos(QoSWithPacketIdentifier::Level0);
                            session.send_publish(&packet, &client_connection_tx);
                        }
                        1 => {
                            packet.set_qos(QoSWithPacketIdentifier::Level1(pkid));
                            session.send_publish(&packet, &client_connection_tx);
                        }
                        2 => {
                            packet.set_qos(QoSWithPacketIdentifier::Level2(pkid));
                            session.send_publish(&packet, &client_connection_tx);
                        }
                        _ => {
                            error!("Invalid qos value: {:?}", qos_val);
                        }
                    }
                }
            }
            ClientSessionMsg::Timeout(payload) => {
                let mut reset_timer = false;
                match &payload {
                    &SessionTimerPayload::RecvPacketTimer(ref packet_type, addr, pkid) => {
                        if let Some(session) = sessions.get_mut(&addr) {
                            let mut publish_packet: Option<(PublishPacket, u8)> = None;
                            if let Some(ref persistent) = session.persistent {
                                match packet_type {
                                    &SessionTimerPacketType::RecvPubackTimeout => {
                                        if let Some(packet) = persistent.qos1_send_packets.get(&pkid) {
                                            // Resend publish packet
                                            publish_packet = Some((packet.clone(), 1));
                                            reset_timer = true;
                                        }
                                    }
                                    &SessionTimerPacketType::RecvPubrecTimeout => {
                                        if let Some(
                                            &(ref packet, _, _)
                                        ) = persistent.qos2_send_packets.get(&pkid) {
                                            // Resend publish packet
                                            publish_packet = Some((packet.clone(), 2));
                                            reset_timer = true;
                                        }
                                    }
                                    &SessionTimerPacketType::RecvPubcompTimeout => {
                                        if persistent.qos2_send_packets.get(&pkid).is_some() {
                                            // Resend pubrel packet
                                            let pubrel = PubrelPacket::new(pkid);
                                            encode_to_client(addr, &pubrel, &client_connection_tx);
                                            reset_timer = true;
                                        }
                                    }
                                    &SessionTimerPacketType::RecvPubrelTimeout => {
                                        if persistent.qos2_recv_packets.get(&pkid).is_some() {
                                            // Resend pubrec packet
                                            let pubrec = PubrecPacket::new(pkid);
                                            encode_to_client(addr, &pubrec, &client_connection_tx);
                                            reset_timer = true;
                                        }
                                    }
                                }
                            }
                            if let Some((mut packet, qos)) = publish_packet {
                                // FIXME: should use this qos?
                                packet.set_dup(true);
                                session.send_publish(&packet, &client_connection_tx);
                            }
                        }
                    }
                    &SessionTimerPayload::KeepAliveTimer(addr) => {
                        let msg = ClientConnectionMsg::DisconnectClient(addr, "Keep-Alive timeout!".to_owned());
                        client_connection_tx.clone().send(msg).wait().unwrap();
                    }
                }

                if reset_timer {
                    let timer_msg = (SessionTimerAction::Set(Instant::now() + recv_packet_timeout), payload);
                    session_timer_tx.send(timer_msg).unwrap();
                }
            }
        };
    }
}

pub struct PersistentSession {
    user_id: u32,
    client_identifier: String,

    // Packet Identifier
    pkid: u16,
    // Fly window for QoS in [1, 2]
    qos1_send_packets: HashMap<u16, PublishPacket>,
    // pkid => (publish, pubrec, pubrel)
    qos2_send_packets: HashMap<u16, (PublishPacket, bool, bool)>,
    // pkid => (publish, pubrec, pubrel)
    qos2_recv_packets: HashMap<u16, (PublishPacket, bool, bool)>
}

impl PersistentSession {

    fn new(user_id: u32, client_identifier: String) -> PersistentSession {
        PersistentSession {
            user_id: user_id,
            client_identifier: client_identifier,
            pkid: 0,
            qos1_send_packets: HashMap::new(),
            qos2_send_packets: HashMap::new(),
            qos2_recv_packets: HashMap::new(),
        }
    }

    fn incr_pkid(&mut self) -> u16 {
        if self.pkid == 65535 {
            self.pkid = 0;
        }
        self.pkid += 1;
        let used = self.qos1_send_packets.contains_key(&self.pkid) ||
            self.qos2_send_packets.contains_key(&self.pkid) ||
            self.qos2_recv_packets.contains_key(&self.pkid);
        if used { self.incr_pkid() } else { self.pkid }
    }

}

pub struct Session {
    buf: EasyBuf,
    addr: SocketAddr,
    recv_packet_timeout: Duration,
    keep_alive_timeout: Duration,
    connected: bool,
    connect_packet: Option<ConnectPacket>,
    fixed_header: Option<FixedHeader>,
    persistent: Option<PersistentSession>,
    session_timer_tx: Sender<(SessionTimerAction, SessionTimerPayload)>,
    client_connection_tx: mpsc::Sender<ClientConnectionMsg>,
    local_router_tx: Sender<LocalRouterMsg>,
    global_retain_tx: Sender<GlobalRetainMsg>
}

impl Session {

    fn new(addr: SocketAddr,
           recv_packet_timeout: Duration,
           keep_alive_timeout: Duration,
           session_timer_tx: Sender<(SessionTimerAction, SessionTimerPayload)>,
           client_connection_tx: mpsc::Sender<ClientConnectionMsg>,
           local_router_tx: Sender<LocalRouterMsg>,
           global_retain_tx: Sender<GlobalRetainMsg>) -> Session {
        Session {
            buf: EasyBuf::new(),
            addr: addr,
            recv_packet_timeout: recv_packet_timeout,
            keep_alive_timeout: keep_alive_timeout,
            fixed_header: None,
            connected: false,
            connect_packet: None,
            persistent: None,
            session_timer_tx: session_timer_tx,
            client_connection_tx: client_connection_tx,
            local_router_tx: local_router_tx,
            global_retain_tx: global_retain_tx
        }
    }

    fn user_id(&self) -> Option<u32> {
        if let Some(ref persistent) = self.persistent {
            Some(persistent.user_id)
        } else { None }
    }

    fn client_identifier(&self) -> Option<String> {
        if let Some(ref persistent) = self.persistent {
            Some(persistent.client_identifier.clone())
        } else { None }
    }

    fn incr_pkid(&mut self) -> u16 {
        if let Some(ref mut persistent) = self.persistent {
            persistent.incr_pkid()
        } else { unreachable!() }
    }

    /// Send PublishPacket to client
    pub fn send_publish(&mut self,
                        packet: &PublishPacket,
                        client_connection_tx: &mpsc::Sender<ClientConnectionMsg>) {
        debug!("[Session.send_publish]: packet={:?}", packet);
        encode_to_client(self.addr, packet, client_connection_tx);
        let mut timer_payload: Option<SessionTimerPayload> = None;
        match packet.qos() {
            QoSWithPacketIdentifier::Level1(pkid) => {
                if let Some(ref mut persistent) = self.persistent {
                    persistent.qos1_send_packets.insert(pkid, packet.clone());
                    timer_payload = Some(SessionTimerPayload::RecvPacketTimer(
                        SessionTimerPacketType::RecvPubackTimeout, self.addr, pkid
                    ));
                }
            }
            QoSWithPacketIdentifier::Level2(pkid) => {
                if let Some(ref mut persistent) = self.persistent {
                    debug!("[Session.send_publish]: INSERT qos2_send_packets(pkid={:?})", pkid);
                    persistent.qos2_send_packets.insert(pkid, (packet.clone(), false, false));
                    timer_payload = Some(SessionTimerPayload::RecvPacketTimer(
                        SessionTimerPacketType::RecvPubrecTimeout, self.addr, pkid
                    ));
                } else {
                    warn!("[Session.send_publish]: self.persistent is None");
                }
            }
            _ => {}
        };

        if let Some(payload) = timer_payload {
            let timer_msg = (SessionTimerAction::Set(Instant::now() + self.recv_packet_timeout), payload);
            self.session_timer_tx.send(timer_msg).unwrap();
        }
    }

    /// Receive PublishPacket from client
    pub fn recv_publish(&mut self, packet: &PublishPacket, is_will: bool) {
        // * Handle RETAIN flag
        // * Forward to `local_router`
        // * [if(qos=1)] Send PubackPacket to Client
        // * [if(qos=2)] Send PubrecPacket to Client
        // * [if(qos > 0)] Add PublishPacket to session.packets:
        //    > [client:PublishPacket]
        let user_id = self.user_id().unwrap();
        if packet.retain() {
            if packet.payload().is_empty() {
                let msg = GlobalRetainMsg::Remove(user_id, packet.topic_name().to_string());
                self.global_retain_tx.send(msg).unwrap();
            } else {
                let msg = GlobalRetainMsg::Insert(user_id, packet.clone());
                self.global_retain_tx.send(msg).unwrap();
            }
        }

        let msg = LocalRouterMsg::ForwardPublish(user_id, packet.clone());
        self.local_router_tx.send(msg).unwrap();

        if !is_will {
            match packet.qos() {
                QoSWithPacketIdentifier::Level0 => {},
                QoSWithPacketIdentifier::Level1(pkid) => {
                    let pkt = PubackPacket::new(pkid);
                    encode_to_client(self.addr, &pkt, &self.client_connection_tx);
                }
                QoSWithPacketIdentifier::Level2(pkid) => {
                    let pkt = PubrecPacket::new(pkid);
                    encode_to_client(self.addr, &pkt, &self.client_connection_tx);
                    if let Some(ref mut persistent) = self.persistent {
                        persistent.qos2_recv_packets.insert(pkid, (packet.clone(), true, false));
                        let timer_payload = SessionTimerPayload::RecvPacketTimer(
                            SessionTimerPacketType::RecvPubrelTimeout, self.addr, pkid
                        );
                        let timer_msg = (SessionTimerAction::Set(
                            Instant::now() + self.recv_packet_timeout), timer_payload);
                        self.session_timer_tx.send(timer_msg).unwrap();
                    } else {
                        unreachable!()
                    }
                }
            };
        }
    }

    pub fn redelivery_packets(&mut self) {
        let persistent = self.persistent.take();
        if let Some(mut persistent) = persistent {
            let client_connection_tx = self.client_connection_tx.clone();
            for packet in persistent.qos1_send_packets.values() {
                self.send_publish(packet, &client_connection_tx);
            }
            for (pkid, &(ref publish, pubrec, pubrel)) in &persistent.qos2_send_packets {
                if pubrec {
                    let pubrel = PubrelPacket::new(*pkid);
                    encode_to_client(self.addr, &pubrel, &client_connection_tx);
                    let timer_payload = SessionTimerPayload::RecvPacketTimer(
                        SessionTimerPacketType::RecvPubcompTimeout, self.addr, *pkid
                    );
                    let timer_msg = (SessionTimerAction::Set(
                        Instant::now() + self.recv_packet_timeout),
                                     timer_payload);
                    self.session_timer_tx.send(timer_msg).unwrap();
                } else {
                    self.send_publish(&publish, &client_connection_tx);
                }
            }
            let mut comp_items = Vec::new();
            for (pkid, &(_, pubrec, pubrel)) in &persistent.qos2_recv_packets {
                if pubrel {
                    // Send pubcomp
                    comp_items.push(*pkid);
                    let pubcomp = PubcompPacket::new(*pkid);
                    encode_to_client(self.addr, &pubcomp, &client_connection_tx);
                } else {
                    // Send pubrec
                    let pkt = PubrecPacket::new(*pkid);
                    encode_to_client(self.addr, &pkt, &client_connection_tx);
                    let timer_payload = SessionTimerPayload::RecvPacketTimer(
                        SessionTimerPacketType::RecvPubrelTimeout, self.addr, *pkid
                    );
                    let timer_msg = (SessionTimerAction::Set(
                        Instant::now() + self.recv_packet_timeout), timer_payload);
                    self.session_timer_tx.send(timer_msg).unwrap();
                }
            }
            for pkid in comp_items {
                persistent.qos2_recv_packets.remove(&pkid);
            }
            self.persistent = Some(persistent);
        }
    }

    pub fn recv_packet(&mut self,
                       addrs: &mut HashMap<(u32, String), SocketAddr>,
                       persistents: &mut HashMap<(u32, String), PersistentSession>,
                       addr: SocketAddr, data: Vec<u8>) {
        // Append data to session.buf
        self.buf.get_mut().extend_from_slice(data.as_slice());
        let mut decoded_packet_count = 0;
        loop {
            if self.fixed_header.is_none() {
                let header_len = {
                    // Valid fixed header length: [1, 5]
                    let buf_slice = self.buf.as_slice();
                    // [Values]:
                    //   None    => No enough bytes
                    //   Some(6) => Error fixed header
                    let mut rv: Option<usize> = None;
                    for i in 1..6 {
                        if buf_slice.len() < i+1 {
                            break
                        }
                        if buf_slice[i] & 0x80 == 0 {
                            rv = Some(i + 1);
                            break
                        }
                    }
                    rv
                };
                if let Some(header_len) = header_len {
                    if header_len >= 6 {
                        // Error fixed header, close Client!
                        let msg = ClientConnectionMsg::DisconnectClient(addr, "Invalid header length!".to_owned());
                        self.client_connection_tx.clone().send(msg).wait().unwrap();
                        break;
                    }
                    // Decode packet fixed_header
                    let header_buf = self.buf.drain_to(header_len);
                    let mut bytes = header_buf.as_ref();
                    self.fixed_header = Some(FixedHeader::decode_with(&mut bytes, None).unwrap())
                } else {
                    // debug!("Not enough bytes to decode FixedHeader: length={}", self.buf.len());
                    break;
                }
            }

            match self.fixed_header {
                Some(fixed_header) => {
                    let remaining_length = fixed_header.remaining_length as usize;
                    if remaining_length <= self.buf.len() {
                        // Decode packet payload
                        let payload_buf = self.buf.drain_to(remaining_length);
                        let mut bytes = payload_buf.as_ref();
                        let packet = VariablePacket::decode_with(&mut bytes, Some(fixed_header)).unwrap();
                        decoded_packet_count += 1;
                        debug!("[>>>>]: {:?}", packet);

                        if let VariablePacket::ConnectPacket(pkt) = packet {
                            // * Send ConnackPacket to Client
                            if self.connected {
                                // <Spec>:
                                // ======
                                // The Server MUST process a second CONNECT Packet
                                // sent from a Client as a protocol violation and
                                // disconnect the Client [MQTT-3.1.0-2]
                                let msg = ClientConnectionMsg::DisconnectClient(addr, "Client already connected!".to_owned());
                                // TODO: Maybe have performace issue!
                                self.client_connection_tx.clone().send(msg).wait().unwrap();
                            } else if pkt.protocol_name() != "MQTT" {
                                let msg = ClientConnectionMsg::DisconnectClient(
                                    addr, format!("Invalid protocol name: {:?}!", pkt.protocol_name()));
                                self.client_connection_tx.clone().send(msg).wait().unwrap();
                            } else {
                                // TODO: how to set `user_id` ???
                                let user_id = 0;
                                let client_identifier = pkt.client_identifier().to_string();
                                self.connected = true;
                                let persistent = persistents.remove(&(user_id, client_identifier.clone()));
                                self.persistent = if !pkt.clean_session() && persistent.is_some() {
                                    info!("[ConnectPacket]: use OLD persistent, client_id={:?}", client_identifier);
                                    persistent
                                } else {
                                    info!("[ConnectPacket]: use NEW persistent, client_id={:?}", client_identifier);
                                    Some(PersistentSession::new(user_id, client_identifier.clone()))
                                };
                                addrs.insert((user_id, client_identifier), addr);
                                let keep_alive = (pkt.keep_alive() as f64 * 1.5 ) as u64;
                                if keep_alive > 0 && keep_alive < self.keep_alive_timeout.as_secs() {
                                    self.keep_alive_timeout = Duration::from_secs(keep_alive);
                                }
                                self.connect_packet = Some(pkt);
                                let connack = ConnackPacket::new(false, ConnectReturnCode::ConnectionAccepted);
                                encode_to_client(addr, &connack, &self.client_connection_tx);
                                self.redelivery_packets();
                            }
                        } else if !self.connected {
                            let msg = ClientConnectionMsg::DisconnectClient(addr, "Client not connected yet!".to_owned());
                            self.client_connection_tx.clone().send(msg).wait().unwrap();
                        } else {
                            match packet {
                                VariablePacket::ConnectPacket(_) => unreachable!(),
                                VariablePacket::PublishPacket(pkt) => {
                                    self.recv_publish(&pkt, false);
                                }

                                // (QoS == 1)
                                VariablePacket::PubackPacket(pkt) => {
                                    // * Remove related sending PublishPacket in self.packets:
                                    //    > [server.PublishPacket]
                                    if let Some(ref mut persistent) = self.persistent {
                                        let pkid = pkt.packet_identifier();
                                        let timer_msg = (
                                            SessionTimerAction::Cancel,
                                            SessionTimerPayload::RecvPacketTimer(
                                                SessionTimerPacketType::RecvPubackTimeout, self.addr, pkid
                                            )
                                        );
                                        self.session_timer_tx.send(timer_msg).unwrap();
                                        persistent.qos1_send_packets.remove(&pkid);
                                    }
                                }

                                // (QoS == 2)
                                VariablePacket::PubrecPacket(pkt) => {
                                    // * Send PubrelPacket to Client
                                    // * Append related packets to self.packets:
                                    //    > [server:PublishPacket] + [client:PubrecPacket, server:PubrelPacket]
                                    let pkid = pkt.packet_identifier();
                                    if let Some(ref mut persistent) = self.persistent {
                                        let timer_msg = (
                                            SessionTimerAction::Cancel,
                                            SessionTimerPayload::RecvPacketTimer(
                                                SessionTimerPacketType::RecvPubrecTimeout, self.addr, pkid
                                            )
                                        );
                                        self.session_timer_tx.send(timer_msg).unwrap();

                                        match persistent.qos2_send_packets.remove(&pkid) {
                                            Some((publish_pkt, _, _)) => {
                                                debug!("[PubrecPacket]: Remove qos2_send_packets(pkid={:?})", pkid);
                                                let pubrel = PubrelPacket::new(pkid);
                                                encode_to_client(addr, &pubrel, &(self.client_connection_tx));
                                                persistent.qos2_send_packets.insert(pkid, (publish_pkt, true, true));
                                                let timer_payload = SessionTimerPayload::RecvPacketTimer(
                                                    SessionTimerPacketType::RecvPubcompTimeout, self.addr, pkid
                                                );
                                                let timer_msg = (SessionTimerAction::Set(
                                                    Instant::now() + self.recv_packet_timeout),
                                                                 timer_payload);
                                                self.session_timer_tx.send(timer_msg).unwrap();
                                            }
                                            None => {
                                                // Send PUBREC twice, just ignore it.
                                                warn!("[PubrecPacket]: Send PUBREC twice, just ignore it.(pikd={:?})", pkid);
                                            }
                                        };
                                    } else {
                                        warn!("[PubrecPacket]: self.persistent not found!");
                                    }
                                }
                                VariablePacket::PubrelPacket(pkt) => {
                                    // * Send PubcompPacket to Client
                                    // * Remove related packets in session.packets:
                                    //    > [client:PublishPacket, server:PubrecPacket]
                                    let pkid = pkt.packet_identifier();
                                    if let Some(ref mut persistent) = self.persistent {
                                        let timer_msg = (
                                            SessionTimerAction::Cancel,
                                            SessionTimerPayload::RecvPacketTimer(
                                                SessionTimerPacketType::RecvPubrelTimeout, self.addr, pkid
                                            )
                                        );
                                        self.session_timer_tx.send(timer_msg).unwrap();
                                        persistent.qos2_recv_packets.remove(&pkid).unwrap();
                                    }
                                    let pubcomp = PubcompPacket::new(pkid);
                                    encode_to_client(addr, &pubcomp, &self.client_connection_tx);
                                }
                                VariablePacket::PubcompPacket(pkt) => {
                                    // * Remove related packets in session.packets:
                                    //    > [server:PublishPacket, client:PubrecPacket, server:PubrelPacket]
                                    let pkid = pkt.packet_identifier();
                                    if let Some(ref mut persistent) = self.persistent {
                                        let timer_msg = (
                                            SessionTimerAction::Cancel,
                                            SessionTimerPayload::RecvPacketTimer(
                                                SessionTimerPacketType::RecvPubcompTimeout, self.addr, pkid
                                            )
                                        );
                                        self.session_timer_tx.send(timer_msg).unwrap();
                                        debug!("[PubcompPacket]: Remove qos2_send_packets(pkid={:?})", pkid);
                                        persistent.qos2_send_packets.remove(&pkid).unwrap();
                                    }
                                }

                                VariablePacket::PingreqPacket(_) => {
                                    // * Send PingrespPacket to Client
                                    // * Update timer
                                    let pingresp = PingrespPacket::new();
                                    encode_to_client(addr, &pingresp, &(self.client_connection_tx));
                                }
                                VariablePacket::DisconnectPacket(_) => {
                                    // * Remove related information in `local_router`
                                    // * Remove current session
                                    // * Close Client connection
                                    // * Remove connect packet' last will
                                    if let Some(ref mut packet) = self.connect_packet {
                                        packet.set_will(None);
                                        let msg = ClientConnectionMsg::DisconnectClient(addr, "Receive disconnect packet!".to_owned());
                                        self.client_connection_tx.clone().send(msg).wait().unwrap();
                                    } else { unreachable!() }
                                }
                                VariablePacket::SubscribePacket(pkt) => {
                                    // * Forward to `local_router`
                                    // * Send suback to client

                                    // <Spec.SUBACK>:
                                    // =============
                                    // When the Server receives a SUBSCRIBE Packet
                                    // from a Client, the Server MUST respond with a
                                    // SUBACK Packet [MQTT-3.8.4-1].

                                    let msg = LocalRouterMsg::Subscribe(
                                        self.user_id().unwrap(), self.client_identifier().unwrap(), addr, pkt.clone());
                                    self.local_router_tx.send(msg).unwrap();
                                    let return_codes = pkt
                                        .payload()
                                        .subscribes()
                                        .iter()
                                        .map(|&(_, ref qos)| {
                                            // TODO: Should check subscribe permissions here
                                            match qos {
                                                &QualityOfService::Level0 => SubscribeReturnCode::MaximumQoSLevel0,
                                                &QualityOfService::Level1 => SubscribeReturnCode::MaximumQoSLevel1,
                                                &QualityOfService::Level2 => SubscribeReturnCode::MaximumQoSLevel2
                                            }
                                        }).collect::<Vec<_>>();

                                    // <Spec.SUBACK>:
                                    // =============
                                    // The SUBACK Packet MUST have the same Packet
                                    // Identifier as the SUBSCRIBE Packet that it is
                                    // acknowledging [MQTT-3.8.4-2].
                                    let suback = SubackPacket::new(pkt.packet_identifier(), return_codes);
                                    encode_to_client(addr, &suback, &(self.client_connection_tx));
                                }
                                VariablePacket::UnsubscribePacket(pkt) => {
                                    // * Forward to `local_router`
                                    // * Send unsuback to client

                                    // <Spec>:
                                    // ======
                                    // The Server MUST respond to an UNSUBSUBCRIBE
                                    // request by sending an UNSUBACK packet. The
                                    // UNSUBACK Packet MUST have the same Packet
                                    // Identifier as the UNSUBSCRIBE Packet
                                    // [MQTT-3.10.4-4].
                                    // Even where no Topic Subscriptions are
                                    // deleted, the Server MUST respond with an
                                    // UNSUBACK [MQTT-3.10.4-5].
                                    let pkid = pkt.packet_identifier();
                                    let msg = LocalRouterMsg::Unsubscribe(self.user_id().unwrap(),
                                                                          self.client_identifier().unwrap(), pkt);
                                    self.local_router_tx.send(msg).unwrap();
                                    let unsuback = UnsubackPacket::new(pkid);
                                    encode_to_client(addr, &unsuback, &(self.client_connection_tx));
                                }

                                // Error cases
                                VariablePacket::ConnackPacket(_) |
                                VariablePacket::PingrespPacket(_) |
                                VariablePacket::SubackPacket(_) |
                                VariablePacket::UnsubackPacket(_) => {
                                    // * Invalid packet(only broker can send those packets), close Client
                                    let msg = ClientConnectionMsg::DisconnectClient(addr, "Can't send broker only packets!".to_owned());
                                    self.client_connection_tx.clone().send(msg).wait().unwrap();
                                }
                            };
                        };
                        self.fixed_header = None;
                    } else {
                        // debug!("Not enough bytes to decode Packet: length={}, fixed_header={:?}",
                        //        self.buf.len(), self.fixed_header.unwrap());
                        break;
                    }
                }
                None => {
                    // Just waiting for more data....
                    panic!("Should break earlier!");
                }
            };
        }

        if decoded_packet_count > 0 {
            // Reset keep alive timeout
            let timer_msg = (SessionTimerAction::Set(Instant::now() + self.keep_alive_timeout),
                             SessionTimerPayload::KeepAliveTimer(addr));
            self.session_timer_tx.send(timer_msg).unwrap();
        }
    }
}

pub fn encode_to_client<'a, T>(
    addr: SocketAddr,
    packet: &'a T,
    client_connection_tx: &mpsc::Sender<ClientConnectionMsg>)
    where T: Packet<'a> + fmt::Debug + 'a
{
    debug!("[<<<<]: {:?}", packet);
    let mut buf = Vec::new();
    packet.encode(&mut buf).unwrap();
    let msg = ClientConnectionMsg::Data(addr, buf);
    client_connection_tx.clone().send(msg).wait().unwrap();
}
