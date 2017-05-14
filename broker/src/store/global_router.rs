
use std::collections::HashSet;
use std::sync::mpsc::{Sender, Receiver};

use mqtt::{TopicName, TopicFilter, QualityOfService};

use common::{UserId, RouteKey, Topic, PeerMap, PeerSet, Routes};
use super::{HubNode, GlobalRouterMsg, StoreSessionMsg};

type GlobalRoutes = Routes<PeerSet<HubNode>, HubNode, ()>;


pub fn run_epoll(
    global_router_rx: Receiver<GlobalRouterMsg>,
    store_session_tx: Sender<StoreSessionMsg>
) {
    let mut global_routes = GlobalRoutes::new();
    loop {
        let msg = global_router_rx.recv().unwrap();
        match msg {
            GlobalRouterMsg::Publish(addr, user_id, packet) => {
                let topic_name = TopicName::new(packet.topic_name().to_string()).unwrap();
                let nodes = global_routes.search(user_id, &topic_name);
                for node in nodes.inner {
                    let msg = StoreSessionMsg::Publish(node.0, user_id, packet.clone());
                    store_session_tx.send(msg).unwrap();
                }
            }
            GlobalRouterMsg::Subscribe(addr, user_id, topics) => {
                let node = HubNode(addr);
                for topic in topics {
                    let _ = global_routes.insert_topic(user_id, &topic, &node, node.clone());
                }
            }
            GlobalRouterMsg::Unsubscribe(addr, user_id, topics) => {
                let node = HubNode(addr);
                for topic in topics {
                    global_routes.remove_topic(user_id, &topic, &node);
                }
            }
            GlobalRouterMsg::NodeDisconnect(addr) => {
                let node = HubNode(addr);
                // HOLD:
            }
        }
    }
}


