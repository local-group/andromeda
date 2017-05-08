
pub mod client_connection;
pub mod client_session;
pub mod local_router;
pub mod router_follower;
pub mod router_leader;
pub mod session_timer;

use std::fmt;
use std::net::SocketAddr;
use std::time::Instant;

use mqtt::{QualityOfService};
use mqtt::packet::{PublishPacket, SubscribePacket, UnsubscribePacket};

pub struct ClientId {
    pub user_id: u32,
    pub client_identifier: String
}

/// Message structure send to `client_connection`
#[derive(Clone)]
pub enum ClientConnectionMsg {
    Data(SocketAddr, Vec<u8>),
    DisconnectClient(SocketAddr, String)
    // Shutdown
}


#[derive(Clone)]
pub enum ClientSessionMsg {
    Data(SocketAddr, Vec<u8>),
    // (user_id, client_identifier, qos, packet)
    Publish(u32, String, QualityOfService, PublishPacket),
    ClientDisconnect(SocketAddr, String),
    // (user_id, addr, packets, subscribe_qos)
    RetainPackets(u32, SocketAddr, Vec<PublishPacket>, QualityOfService),
    Timeout(SessionTimerPayload)
    // Shutdown
}

#[derive(Debug, Clone)]
pub enum LocalRouterMsg {
    // Forward publish message to `router_follower` or `local_router`
    ForwardPublish(u32, PublishPacket),
    // Receive publish packet from `router_follower` or `local_router`
    Publish(u32, PublishPacket),
    // (user_id, client_identifier, packet)
    Subscribe(u32, String, SocketAddr, SubscribePacket),
    Unsubscribe(u32, String, UnsubscribePacket),
    ClientDisconnect(u32, String),
    // Shutdown
}

#[derive(Debug, Clone)]
pub enum RouterFollowerMsg {
    _Shutdown
}

#[derive(Debug, Clone)]
pub enum RouterLeaderMsg {
    _Shutdown
}

#[derive(Debug, Copy, Clone)]
pub enum SessionTimerAction {
    Set(Instant), Cancel
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub enum SessionTimerPacketType {
    // [QoS.1.send] Receive PUBACK timeout
    RecvPubackTimeout,
    // [QoS.2.send] Receive PUBREC timeout
    RecvPubrecTimeout,
    // [QoS.2.send] Receive PUBCOMP timeout
    RecvPubcompTimeout,
    // [QoS.2.recv] Receive PUBREL timeout
    RecvPubrelTimeout,
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub enum SessionTimerPayload {
    // SocketAddr => Client addr
    // u16 => packet_identifier (pkid)
    RecvPacketTimer(SessionTimerPacketType, SocketAddr, u16),
    // Receive PINGREQ timeout
    KeepAliveTimer(SocketAddr),
    // Decode one packet timeout (maybe useless ??)
    // DecodePacketTimer(SocketAddr),
}

/// impl Debug for structures
impl fmt::Debug for ClientConnectionMsg {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            &ClientConnectionMsg::Data(addr, ref bytes) => {
                write!(f, "Write <<{} bytes>> to client [{:?}]", bytes.len(), addr)
            }
            &ClientConnectionMsg::DisconnectClient(addr, ref reason) => {
                write!(f, "Disconnect [{:?}] because: <<{}>>", addr, reason)
            }
        }
    }
}
