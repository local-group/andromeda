
use mqtt::{TopicName, TopicFilter, QualityOfService};

use common::{UserId, RouteKey, Topic, PeerMap, PeerSet, Routes};
use super::{HubNode};

pub fn run_epoll() {
    // HOLD:
}

type GlobalRoutes = Routes<PeerSet<HubNode>, HubNode, ()>;
