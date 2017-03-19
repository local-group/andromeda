
extern crate mqtt;
extern crate native_tls;

use std::thread::{self, JoinHandle};
use std::io::{Write};
use std::sync::mpsc::{channel, Receiver};
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs, TcpStream, Shutdown};
use std::time::{Duration};

use native_tls::TlsConnector;

use mqtt::{Encodable, Decodable, TopicFilter, TopicName, QualityOfService};
use mqtt::packet::{QoSWithPacketIdentifier, VariablePacket,
                   ConnectPacket, PingreqPacket,
                   SubscribePacket, UnsubscribePacket, PublishPacket};

pub enum ClientPingMsg {}
pub enum ClientRecvMsg {}

pub enum ClientInboxMsg {
    Received(VariablePacket),
    Shutdown
}

pub struct Client {
    addr: SocketAddr,
    tls_connector: TlsConnector,
    keep_alive: u32,
    client_identifier: String,
    clean_session: bool,
    ignore_pingresp: bool,
    will: Option<(TopicName, Vec<u8>)>,
    will_qos: Option<u8>,
    will_retain: Option<bool>,
    pkid: u16,
    started: bool,
    inbox: Option<Receiver<VariablePacket>>,
    stream: Option<TcpStream>,
    ping_thread: Option<thread::JoinHandle<()>>,
    recv_thread: Option<thread::JoinHandle<()>>,
}

impl Client {
    pub fn new(url: &str,       // ssl://140.205.202.2:8080
               tls_connector: TlsConnector,
               keep_alive: u32,
               client_identifier: &str, clean_session: bool,
               ignore_pingresp: bool,
               will: Option<(&str, Vec<u8>)>,
               will_qos: Option<u8>, will_retain: Option<bool>) -> Client {
        let will = will.map(|(topic, message)| {
            (TopicName::new(topic.to_string()).unwrap(), message)
        });
        let parts: Vec<&str> = url.split("://").collect();
        let _schema = parts[0];
        let server_port = parts[1];
        let addr = server_port.to_socket_addrs().unwrap().next().unwrap();

        Client {
            addr: addr,
            tls_connector: tls_connector,
            keep_alive: keep_alive,
            client_identifier: client_identifier.to_string(),
            clean_session: clean_session,
            ignore_pingresp: ignore_pingresp,
            will: will,
            will_qos: will_qos,
            will_retain: will_retain,
            pkid: 0,
            started: false,
            inbox: None,
            stream: None,
            ping_thread: None,
            recv_thread: None,
        }
    }

    pub fn connect<F>(&mut self, retry: usize, handler: F)
        where F: FnOnce(&mut Client, &Receiver<VariablePacket>) -> (),
              F: Send + 'static
    {
        println!("[{}]: connecting to {:?}", self.client_identifier, self.addr);
        let stream = {
            let mut stream = TcpStream::connect(self.addr);
            for _ in 0..retry {
                if stream.is_err() {
                    thread::sleep(Duration::from_millis(100));
                    stream = TcpStream::connect(self.addr);
                } else {
                    break;
                }
            }
            stream.unwrap()
        };

        println!("[{}]: connected {:?}", self.client_identifier, self.addr);
        let (inbox_sender, inbox) = channel::<VariablePacket>();

        let mut cloned_stream = stream.try_clone().unwrap();
        let cloned_client_identifier = self.client_identifier.clone();
        let keep_alive = self.keep_alive;
        let ping_thread = thread::spawn(move || {
            let sleep_time = (keep_alive as f32 * 0.9) as u64;
            loop {
                // TODO: reset timer when send packet successfully!
                thread::sleep(Duration::from_secs(sleep_time));
                let pingreq_packet = PingreqPacket::new();
                let mut buf = Vec::new();
                pingreq_packet.encode(&mut buf).unwrap();
                if cloned_stream.write_all(&buf[..]).is_err() {
                    break;
                }
            }
            println!("[{}]: Ping thread quit", cloned_client_identifier);
        });

        let mut cloned_stream = stream.try_clone().unwrap();
        let cloned_inbox_sender = inbox_sender.clone();
        let ignore_pingresp = self.ignore_pingresp;
        let cloned_client_identifier = self.client_identifier.clone();
        let recv_thread = thread::spawn(move || {
            loop {
                let packet = match VariablePacket::decode(&mut cloned_stream) {
                    Ok(pkt) => {
                        let ignore = match pkt {
                            VariablePacket::PingrespPacket(_) => ignore_pingresp,
                            _ => false
                        };
                        if ignore { None} else { Some(pkt) }
                    },
                    // Print error message
                    Err(_) => {
                        println!("[{}] Recv thread error", cloned_client_identifier);
                        None
                    }
                };
                if let Some(pkt) = packet  {
                    if cloned_inbox_sender.send(pkt).is_err() {
                        break;
                    }
                } else {
                    break;
                }
            }
            println!("[{}]: Recv thread quit", cloned_client_identifier);
        });

        let mut cloned_stream = stream.try_clone().unwrap();
        self.started = true;
        self.stream = Some(stream);
        self.ping_thread = Some(ping_thread);
        self.recv_thread = Some(recv_thread);

        let mut connect_pkt = ConnectPacket::new("MQTT".to_owned(), self.client_identifier.clone());
        connect_pkt.set_clean_session(self.clean_session);
        connect_pkt.set_keep_alive(self.keep_alive as u16);
        if self.will.is_some() {
            connect_pkt.set_will(self.will.clone());
            connect_pkt.set_will_qos(self.will_qos.unwrap_or(0));
            connect_pkt.set_will_retain(self.will_retain.unwrap_or(false));
        }
        println!("[{}]: sending connect packet {:?}", self.client_identifier, connect_pkt);
        Client::send_packet(&mut cloned_stream, &VariablePacket::ConnectPacket(connect_pkt));
        println!("[{}]: connect packet sent!", self.client_identifier);
        handler(self, &inbox);
        self.inbox = Some(inbox);
    }

    pub fn shutdown(&self) {
        println!("[{}] Shutdown client......", self.client_identifier);
        if let Some(ref stream) = self.stream {
            let rv = stream.shutdown(Shutdown::Both);
            println!("[{}]: client shutdown={:?}", self.client_identifier, rv);
        }
    }

    pub fn subscribe(&mut self, subscriptions: Vec<(&str, u32)>) {
        let subscriptions = subscriptions.iter().map(|&(topic, qos)| {
            let qos = match qos {
                0 => QualityOfService::Level0,
                1 => QualityOfService::Level1,
                2 => QualityOfService::Level2,
                _ => panic!("Invalid qos")
            };
            (TopicFilter::new_checked(topic).unwrap(), qos)
        }).collect::<Vec<(TopicFilter, QualityOfService)>>();
        let packet = SubscribePacket::new(self.pkid(), subscriptions);
        self.send(VariablePacket::SubscribePacket(packet));
    }

    pub fn unsubscribe(&mut self, subscriptions: Vec<&str>) {
        let subscriptions = subscriptions.iter().map(|topic| {
            TopicFilter::new_checked(*topic).unwrap()
        }).collect::<Vec<TopicFilter>>();
        let packet = UnsubscribePacket::new(self.pkid(), subscriptions);
        self.send(VariablePacket::UnsubscribePacket(packet));
    }

    pub fn publish(&mut self, topic_name: &str, payload: Vec<u8>, qos: u32, retain: bool, dup: bool) {
        let topic_name = TopicName::new(topic_name.to_string()).unwrap();
        let pkid = self.pkid();
        let qos = match qos {
            0 => QoSWithPacketIdentifier::Level0,
            1 => QoSWithPacketIdentifier::Level1(pkid),
            2 => QoSWithPacketIdentifier::Level2(pkid),
            _ => panic!("Invalid qos")
        };
        let mut packet = PublishPacket::new(topic_name, qos, payload);
        packet.set_retain(retain);
        packet.set_dup(dup);
        self.send(VariablePacket::PublishPacket(packet));
    }

    fn pkid(&mut self) -> u16 {
        if self.pkid == 65535 {
            self.pkid = 0;
        }
        self.pkid += 1;
        self.pkid
    }

    pub fn send_packet(stream: &mut TcpStream, packet: &VariablePacket) {
        let mut buf = Vec::new();
        packet.encode(&mut buf).unwrap();
        stream.write_all(&buf[..]).unwrap();
    }

    pub fn send(&mut self, packet: VariablePacket) {
        if !self.started {
            panic!("Client threads not started!");
        }
        if let Some(ref mut stream) = self.stream {
            Client::send_packet(stream, &packet);
        }
    }

    pub fn receive(&mut self) -> Option<VariablePacket> {
        match self.inbox {
            Some(ref inbox) => Some(inbox.recv().unwrap()),
            None => None
        }
    }
}
