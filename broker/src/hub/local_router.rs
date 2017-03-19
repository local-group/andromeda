
use std::net::SocketAddr;
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Sender, Receiver};

use futures::sync::mpsc;

use mqtt::packet::{Packet};
use mqtt::{TopicFilter, TopicName, QualityOfService};

use common::{Topic};
use store::{GlobalRetainMsg};
use super::{ClientSessionMsg, LocalRouterMsg};


pub fn run(
    local_router_rx: Receiver<super::LocalRouterMsg>,
    local_router_tx: Sender<super::LocalRouterMsg>,
    client_session_tx: Sender<super::ClientSessionMsg>,
    _router_follower_tx: mpsc::Sender<super::RouterFollowerMsg>,
    _router_leader_tx: mpsc::Sender<super::RouterLeaderMsg>,
    global_retain_tx: Sender<GlobalRetainMsg>,
) {
    let mut local_routes = LocalRoutes::new();
    loop {
        let msg = local_router_rx.recv().unwrap();
        match msg {
            LocalRouterMsg::ForwardPublish(user_id, packet) => {
                // Forward message to current receiver.
                local_router_tx.send(LocalRouterMsg::Publish(user_id, packet)).unwrap();
            }
            LocalRouterMsg::Publish(user_id, packet) => {
                let topic_name = TopicName::new(packet.topic_name().to_string()).unwrap();
                let clients: HashMap<_, _> = local_routes.search(user_id, &topic_name);
                for (addr, qos) in clients {
                    client_session_tx.send(ClientSessionMsg::Publish(addr, qos, packet.clone())).unwrap();
                }
            }
            LocalRouterMsg::Subscribe(user_id, addr, packet) => {
                for &(ref topic_filter, qos) in packet.payload().subscribes() {
                    let topic = Topic::from_filter(topic_filter);
                    let first_subscribe = local_routes.insert_topic(user_id, &topic, addr, qos);
                    if first_subscribe {
                        let msg = GlobalRetainMsg::MatchAll(user_id, addr, topic, qos);
                        global_retain_tx.send(msg).unwrap();
                    }
                }
            }
            LocalRouterMsg::Unsubscribe(user_id, addr, packet) => {
                for topic_filter in packet.payload().subscribes() {
                    let topic = Topic::from_filter(topic_filter);
                    local_routes.remove_topic(user_id, &topic, addr);
                }
            }
            LocalRouterMsg::ClientDisconnect(user_id, addr) => {
                local_routes.remove_all_topics(user_id, addr);
            }
        }
    }
}



/// Just for topic filters
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RouteKey {
    Normal(String),
    OneLevel,
    RestLevels,
}

impl<'a> From<&'a str> for RouteKey {
    fn from(token: &'a str) -> RouteKey {
        match token {
            "+" => RouteKey::OneLevel,
            "#" => RouteKey::RestLevels,
            s @ _ => RouteKey::Normal(s.to_string())
        }
    }
}

// type MqttClient = (u32, SocketAddr);

// TODO:
// ====
//  * Slow now, use `nikomatsakis/rayon` to speed up!

#[derive(Debug)]
pub struct LocalRouteNode {
    children: Option<HashMap<RouteKey, LocalRouteNode>>,
    clients: Option<HashMap<SocketAddr, QualityOfService>>
}

impl LocalRouteNode {
    pub fn new () -> LocalRouteNode {
        LocalRouteNode {
            children: None,
            clients: None
        }
    }

    pub fn insert(&mut self, tf: &TopicFilter, addr: SocketAddr, qos: QualityOfService) -> bool {
        let mut last_node = (*tf).split("/").fold(self, |current, token| {
            let token = RouteKey::from(token);
            if current.children.is_none() {
                current.children = Some(HashMap::<RouteKey, LocalRouteNode>::new());
            }
            match current.children {
                Some(ref mut map) => {
                    if !map.contains_key(&token) {
                        map.insert(token.clone(), LocalRouteNode::new());
                    }
                    map.get_mut(&token).unwrap()
                }
                _ => unreachable!()
            }
        });

        if last_node.clients.is_none() {
            last_node.clients = Some(HashMap::<SocketAddr, QualityOfService>::new())
        }
        match last_node.clients {
            Some(ref mut client_map) => {
                client_map.insert(addr, qos).is_none()
            }
            _ => unreachable!()
        }
    }

    pub fn is_empty(&self) -> bool {
        (match self.clients {
            Some(ref client_map) => client_map.is_empty(),
            None => true
        }) && (match self.children {
            Some(ref map) => map.is_empty(),
            None => true
        })
    }

    fn _remove<'a, I>(&mut self, mut tokens: I, addr: SocketAddr) -> bool
        where I: Iterator<Item=&'a str>
    {
        match tokens.next() {
            Some(token) => {
                match self.children {
                    Some(ref mut map) => {
                        let mut child_empty = false;
                        let key = RouteKey::from(token);
                        let is_removed = match map.get_mut(&key) {
                            Some(node) => {
                                let is_removed = node._remove(tokens, addr);
                                child_empty = node.is_empty();
                                is_removed
                            }
                            None => false
                        };
                        if child_empty {
                            map.remove(&key);
                        }
                        is_removed
                    }
                    None => false
                }
            }
            None => {
                match self.clients {
                    Some(ref mut client_map) => {
                        client_map.remove(&addr).is_some()
                    }
                    None => false
                }
            }
        }
    }

    pub fn remove(&mut self, tf: &TopicFilter, addr: SocketAddr) -> bool {
        self._remove((*tf).split("/"), addr)
    }

    fn _search(&self, tokens: &Vec<&str>, index: usize) -> HashMap<SocketAddr, QualityOfService> {
        let mut clients = HashMap::<SocketAddr, QualityOfService>::new();
        if let Some(token) = tokens.get(index) {
            match self.children {
                Some(ref map) => {
                    if let Some(node) = map.get(&RouteKey::Normal(token.to_string())) {
                        clients.extend(node._search(tokens, index+1));
                    }
                    if let Some(node) = map.get(&RouteKey::OneLevel) {
                        clients.extend(node._search(tokens, index+1));
                    }
                    if let Some(node) = map.get(&RouteKey::RestLevels) {
                        if let Some(ref new_clients) = node.clients {
                            clients.extend(new_clients.clone());
                        }
                    }
                }
                None => {}
            }
        } else {
            if let Some(ref new_clients) = self.clients {
                clients.extend(new_clients.clone());
            }
        }
        clients
    }

    pub fn search(&self, name: &TopicName) -> HashMap<SocketAddr, QualityOfService> {
        self._search(&(*name).split("/").collect::<Vec<&str>>(), 0)
    }
}


pub struct LocalRoutes {
    // For topic filters
    filter_routes: HashMap<u32, LocalRouteNode>,
    // For topic names
    name_routes: HashMap<u32, HashMap<TopicName, HashMap<SocketAddr, QualityOfService>>>,
    // For remove topic-names and topic-filters
    client_topics: HashMap<u32, HashMap<SocketAddr, HashSet<Topic>>>,
}

/// Router thread
/// * subscribe a topic
/// * unsubscribe a topic
/// * unsubscribe all topics from a client
/// * match topics
impl LocalRoutes {
    pub fn new() -> LocalRoutes {
        LocalRoutes {
            filter_routes: HashMap::<u32, LocalRouteNode>::new(),
            name_routes: HashMap::<u32, HashMap<TopicName, HashMap<SocketAddr, QualityOfService>>>::new(),
            client_topics: HashMap::<u32, HashMap<SocketAddr, HashSet<Topic>>>::new()
        }
    }

    fn insert_topic(&mut self, user_id: u32, topic: &Topic,
                    addr: SocketAddr, qos: QualityOfService) -> bool {
        if !self.filter_routes.contains_key(&user_id) {
            self.filter_routes.insert(user_id, LocalRouteNode::new());
        }
        if !self.name_routes.contains_key(&user_id) {
            self.name_routes.insert(user_id, HashMap::<TopicName, HashMap<SocketAddr, QualityOfService>>::new());
        }
        if !self.client_topics.contains_key(&user_id) {
            self.client_topics.insert(user_id, HashMap::<SocketAddr, HashSet<Topic>>::new());
        }
        let mut filter_routes = self.filter_routes.get_mut(&user_id).unwrap();
        let mut name_routes = self.name_routes.get_mut(&user_id).unwrap();
        let mut client_topics = self.client_topics.get_mut(&user_id).unwrap();

        if !client_topics.contains_key(&addr) {
            client_topics.insert(addr, HashSet::<Topic>::new());
        }
        client_topics.get_mut(&addr).unwrap()
            .insert(topic.clone());

        match topic {
            &Topic::Filter(ref topic_filter) => {
                filter_routes.insert(topic_filter, addr, qos)
            }
            &Topic::Name(ref topic_name) => {
                if !name_routes.contains_key(topic_name) {
                    name_routes.insert(topic_name.clone(), HashMap::<SocketAddr, QualityOfService>::new());
                }
                name_routes.get_mut(topic_name).unwrap()
                    .insert(addr, qos).is_none()
            }
        }
    }

    fn remove_topic(&mut self, user_id: u32, topic: &Topic, addr: SocketAddr) {
        if let Some(ref mut client_topics) = self.client_topics.get_mut(&user_id) {
            match client_topics.get_mut(&addr) {
                Some(ref mut topic_name_set) => topic_name_set.remove(topic),
                None => false
            };
        }
        match topic {
            &Topic::Filter(ref topic_filter) => {
                if let Some(ref mut filter_routes) = self.filter_routes.get_mut(&user_id) {
                    filter_routes.remove(topic_filter, addr);
                }
            }
            &Topic::Name(ref topic_name) => {
                if let Some(ref mut name_routes) = self.name_routes.get_mut(&user_id) {
                    match name_routes.get_mut(topic_name) {
                        Some(ref mut client_map) => client_map.remove(&addr).is_some(),
                        None => false
                    };
                }
            }
        }
    }

    fn remove_all_topics(&mut self, user_id: u32, addr: SocketAddr) -> usize {
        if let Some(ref mut client_topics) = self.client_topics.get_mut(&user_id) {
            let removed_count = match client_topics.get_mut(&addr) {
                Some(ref mut topic_name_set) => {
                    let mut removed_count = 0;
                    for topic in topic_name_set.iter() {
                        removed_count += match topic {
                            &Topic::Filter(ref topic_filter) => {
                                if let Some(ref mut filter_routes) = self.filter_routes.get_mut(&user_id) {
                                    if filter_routes.remove(topic_filter, addr) { 1 } else { 0 }
                                } else { 0 }
                            }
                            &Topic::Name(ref topic_name) => {
                                if let Some(ref mut name_routes) = self.name_routes.get_mut(&user_id) {
                                    match name_routes.get_mut(topic_name) {
                                        Some(ref mut client_map) => {
                                            if client_map.remove(&addr).is_some() { 1 } else { 0 }
                                        }
                                        None => 0
                                    }
                                } else { 0 }
                            }
                        }
                    }
                    removed_count
                }
                None => 0
            };
            client_topics.remove(&addr);
            removed_count
        } else { 0 }
    }

    fn search(&self, user_id: u32, topic_name: &TopicName) -> HashMap<SocketAddr, QualityOfService> {
        let mut clients = HashMap::<SocketAddr, QualityOfService>::new();
        if let Some(ref name_routes) = self.name_routes.get(&user_id) {
            if let Some(client_map) = name_routes.get(topic_name) {
                clients.extend(client_map);
            }
        }
        if let Some(ref filter_routes) = self.filter_routes.get(&user_id) {
            clients.extend(filter_routes.search(topic_name));
        }
        clients
    }
}
