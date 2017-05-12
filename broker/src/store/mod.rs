
pub mod global_router;
pub mod global_retain;
pub mod global_session;

use std::net::SocketAddr;

use mqtt::{QualityOfService};
use mqtt::packet::{PublishPacket};

use common::{Topic};

#[derive(Debug, Clone)]
pub enum GlobalRetainMsg {
    Insert(u32, PublishPacket),
    Remove(u32, String),
    MatchAll(u32, SocketAddr, Topic, QualityOfService)
}

#[derive(Clone, Hash, Eq, PartialEq)]
pub struct HubNode;
