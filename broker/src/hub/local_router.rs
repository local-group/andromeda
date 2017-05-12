
use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{Sender, Receiver};

use futures::sync::mpsc;

use mqtt::packet::{Packet};
use mqtt::{TopicFilter, TopicName, QualityOfService};

use common::{Topic, RouteKey};
use store::{GlobalRetainMsg};
use super::{ClientSessionMsg, LocalRouterMsg, ClientIdentifier};


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
                for (client_identifier, qos) in clients {
                    client_session_tx.send(ClientSessionMsg::Publish(user_id, client_identifier, qos, packet.clone())).unwrap();
                }
            }
            LocalRouterMsg::Subscribe(user_id, client_identifier, addr, packet) => {
                for &(ref topic_filter, qos) in packet.payload().subscribes() {
                    let topic = Topic::from_filter(topic_filter);
                    let _ = local_routes.insert_topic(user_id, &topic, &client_identifier, qos);
                    // <Spec>: [MQTT-3.8.4-3] "Any existing retained messages matching the Topic Filter MUST be re-sent"
                    let msg = GlobalRetainMsg::MatchAll(user_id, addr, topic, qos);
                    global_retain_tx.send(msg).unwrap();
                }
            }
            LocalRouterMsg::Unsubscribe(user_id, client_identifier, packet) => {
                for topic_filter in packet.payload().subscribes() {
                    let topic = Topic::from_filter(topic_filter);
                    local_routes.remove_topic(user_id, &topic, &client_identifier);
                }
            }
            LocalRouterMsg::ClientDisconnect(user_id, client_identifier) => {
                local_routes.remove_all_topics(user_id, &client_identifier);
            }
        }
    }
}


// TODO:
// ====
//  * Slow now, use `nikomatsakis/rayon` to speed up!


#[derive(Debug)]
pub struct LocalRouteNode {
    children: Option<HashMap<RouteKey, LocalRouteNode>>,
    clients: HashMap<ClientIdentifier, QualityOfService>
}

impl LocalRouteNode {
    pub fn new () -> LocalRouteNode {
        LocalRouteNode {
            children: None,
            clients: HashMap::new()
        }
    }

    pub fn insert(&mut self, tf: &TopicFilter, client_identifier: &ClientIdentifier, qos: QualityOfService) -> bool {
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

        last_node.clients.insert(client_identifier.clone(), qos).is_none()
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty() && (match self.children {
            Some(ref map) => map.is_empty(),
            None => true
        })
    }

    fn _remove<'a, I>(&mut self, mut tokens: I, client_identifier: &ClientIdentifier) -> bool
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
                                let is_removed = node._remove(tokens, client_identifier);
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
                self.clients.remove(client_identifier).is_some()
            }
        }
    }

    pub fn remove(&mut self, tf: &TopicFilter, client_identifier: &ClientIdentifier) -> bool {
        self._remove((*tf).split("/"), client_identifier)
    }

    fn _search(&self, tokens: &Vec<&str>, index: usize) -> HashMap<ClientIdentifier, QualityOfService> {
        let mut clients = HashMap::<ClientIdentifier, QualityOfService>::new();
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
                        clients.extend(node.clients.clone());
                    }
                }
                None => {}
            }
        } else {
            clients.extend(self.clients.clone());
        }
        clients
    }

    pub fn search(&self, name: &TopicName) -> HashMap<ClientIdentifier, QualityOfService> {
        self._search(&(*name).split("/").collect::<Vec<&str>>(), 0)
    }
}


pub struct LocalRoutes {
    // For topic filters
    //   {user_id => routes}
    filter_routes: HashMap<u32, LocalRouteNode>,
    // For topic names
    //   {user_id => topic_names}
    name_routes: HashMap<u32, HashMap<TopicName, HashMap<ClientIdentifier, QualityOfService>>>,
    // For remove topic-names and topic-filters
    //   {user_id => topics}
    client_topics: HashMap<u32, HashMap<ClientIdentifier, HashSet<Topic>>>,
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
            name_routes: HashMap::<u32, HashMap<TopicName, HashMap<ClientIdentifier, QualityOfService>>>::new(),
            client_topics: HashMap::<u32, HashMap<ClientIdentifier, HashSet<Topic>>>::new()
        }
    }

    fn insert_topic(&mut self, user_id: u32, topic: &Topic,
                    client_identifier: &ClientIdentifier, qos: QualityOfService) -> bool {
        if !self.filter_routes.contains_key(&user_id) {
            self.filter_routes.insert(user_id, LocalRouteNode::new());
        }
        if !self.name_routes.contains_key(&user_id) {
            self.name_routes.insert(user_id, HashMap::<TopicName, HashMap<ClientIdentifier, QualityOfService>>::new());
        }
        if !self.client_topics.contains_key(&user_id) {
            self.client_topics.insert(user_id, HashMap::<ClientIdentifier, HashSet<Topic>>::new());
        }
        let mut filter_routes = self.filter_routes.get_mut(&user_id).unwrap();
        let mut name_routes = self.name_routes.get_mut(&user_id).unwrap();
        let mut client_topics = self.client_topics.get_mut(&user_id).unwrap();

        if !client_topics.contains_key(client_identifier) {
            client_topics.insert(client_identifier.clone(), HashSet::<Topic>::new());
        }
        client_topics.get_mut(client_identifier).unwrap()
            .insert(topic.clone());

        match topic {
            &Topic::Filter(ref topic_filter) => {
                filter_routes.insert(topic_filter, client_identifier, qos)
            }
            &Topic::Name(ref topic_name) => {
                if !name_routes.contains_key(topic_name) {
                    name_routes.insert(topic_name.clone(), HashMap::<ClientIdentifier, QualityOfService>::new());
                }
                name_routes.get_mut(topic_name).unwrap()
                    .insert(client_identifier.to_owned(), qos).is_none()
            }
        }
    }

    fn remove_topic(&mut self, user_id: u32, topic: &Topic, client_identifier: &ClientIdentifier) {
        if let Some(ref mut client_topics) = self.client_topics.get_mut(&user_id) {
            match client_topics.get_mut(client_identifier) {
                Some(ref mut topic_name_set) => topic_name_set.remove(topic),
                None => false
            };
        }
        match topic {
            &Topic::Filter(ref topic_filter) => {
                if let Some(ref mut filter_routes) = self.filter_routes.get_mut(&user_id) {
                    filter_routes.remove(topic_filter, client_identifier);
                }
            }
            &Topic::Name(ref topic_name) => {
                if let Some(ref mut name_routes) = self.name_routes.get_mut(&user_id) {
                    match name_routes.get_mut(topic_name) {
                        Some(ref mut client_map) => client_map.remove(client_identifier).is_some(),
                        None => false
                    };
                }
            }
        }
    }

    fn remove_all_topics(&mut self, user_id: u32, client_identifier: &ClientIdentifier) -> usize {
        if let Some(ref mut client_topics) = self.client_topics.get_mut(&user_id) {
            let removed_count = match client_topics.get_mut(client_identifier) {
                Some(ref mut topic_name_set) => {
                    let mut removed_count = 0;
                    for topic in topic_name_set.iter() {
                        removed_count += match topic {
                            &Topic::Filter(ref topic_filter) => {
                                if let Some(ref mut filter_routes) = self.filter_routes.get_mut(&user_id) {
                                    if filter_routes.remove(topic_filter, client_identifier) { 1 } else { 0 }
                                } else { 0 }
                            }
                            &Topic::Name(ref topic_name) => {
                                if let Some(ref mut name_routes) = self.name_routes.get_mut(&user_id) {
                                    match name_routes.get_mut(topic_name) {
                                        Some(ref mut client_map) => {
                                            if client_map.remove(client_identifier).is_some() { 1 } else { 0 }
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
            client_topics.remove(client_identifier);
            removed_count
        } else { 0 }
    }

    fn search(&self, user_id: u32, topic_name: &TopicName) -> HashMap<ClientIdentifier, QualityOfService> {
        let mut clients = HashMap::<ClientIdentifier, QualityOfService>::new();
        if let Some(ref name_routes) = self.name_routes.get(&user_id) {
            if let Some(client_map) = name_routes.get(topic_name) {
                clients.extend(client_map.clone());
            }
        }
        if let Some(ref filter_routes) = self.filter_routes.get(&user_id) {
            clients.extend(filter_routes.search(topic_name));
        }
        clients
    }
}
