
pub mod connection;
pub mod route;

use mqtt::{TopicName, TopicFilter, QualityOfService};
use mqtt::packet::PublishPacket;

pub use self::route::{
    Peers, PeerMap, PeerSet,
    RouteKey, RouteNode, Routes
};
pub use self::connection::{
    MsgFromConnection, ToConnectionMsg,
    ConnectionMgr
};


#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct UserId(pub u32);

#[derive(Debug, Hash, Eq, Serialize, Deserialize, PartialEq, Clone)]
pub struct ClientIdentifier(pub String);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub enum Topic {
    Filter(TopicFilter),
    Name(TopicName)
}

impl Topic {

    pub fn from_filter(filter: &TopicFilter) -> Topic {
        if (*filter).find('+').is_some() || (*filter).find('#').is_some() {
            Topic::Filter(filter.clone())
        } else {
            unsafe { Topic::Name(TopicName::new_unchecked((*filter).to_string())) }
        }
    }

    pub fn from_name(name: &TopicName) -> Topic {
        Topic::Name(name.clone())
    }

    pub fn from_str(topic: &str) -> Topic {
        if topic.find('+').is_some() || topic.find('#').is_some() {
            Topic::Filter(TopicFilter::new(topic))
        } else {
            Topic::Name(TopicName::new(topic.to_string()).unwrap())
        }
    }
}


/******************** TODO: RPC message *************************/
#[derive(Serialize, Deserialize, PartialEq)]
pub enum StoreRequest {
    Publish(UserId, PublishPacket),
    Subscribe(UserId, Vec<Topic>),
    Unsubscribe(UserId, Vec<Topic>),
    GetRetains(UserId, ClientIdentifier, Topic, QualityOfService)
}

#[derive(Serialize, Deserialize, PartialEq)]
pub enum StoreResponse {
    Publish(UserId, PublishPacket),
    Retains(UserId, ClientIdentifier, Vec<PublishPacket>, QualityOfService),
}
