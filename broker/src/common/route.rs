
use std::marker::PhantomData;
use std::hash::Hash;
use std::collections::{hash_set, hash_map, HashMap, HashSet};

use mqtt::{TopicName, TopicFilter, QualityOfService};
use mqtt::packet::PublishPacket;

use super::{Topic, UserId, ClientIdentifier};

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

pub trait Peers<Key, Value>: Clone {
    type Item;
    fn new() -> Self;
    fn insert(&mut self, item: Self::Item) -> bool;
    fn remove(&mut self, key: &Key) -> bool;
    fn contains(&self, key: &Key) -> bool;
    fn is_empty(&self) -> bool;
    fn extend(&mut self, other: Self);
}

#[derive(Clone)]
pub struct PeerMap<Key, Value> {
    pub inner: HashMap<Key, Value>
}

impl<Key, Value> Peers<Key, Value> for PeerMap<Key, Value>
    where Key: Eq + Hash + Clone,
          Value: Clone
{
    type Item = (Key, Value);

    fn new() -> PeerMap<Key, Value> {
        PeerMap {inner: HashMap::new()}
    }
    fn insert(&mut self, item: (Key, Value)) -> bool {
        self.inner.insert(item.0, item.1).is_some()
    }
    fn remove(&mut self, key: &Key) -> bool {
        self.inner.remove(key).is_some()
    }
    fn contains(&self, key: &Key) -> bool {
        self.inner.contains_key(key)
    }
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    fn extend(&mut self, other: PeerMap<Key, Value>) {
        self.inner.extend(other.inner)
    }
}


#[derive(Clone)]
pub struct PeerSet<Key> {
    pub inner: HashSet<Key>
}

impl<Key, Value> Peers<Key, Value> for PeerSet<Key>
    where Key: Eq + Hash + Clone,
{
    type Item = Key;

    fn new() -> PeerSet<Key> {
        PeerSet {inner: HashSet::new()}
    }
    fn insert(&mut self, item: Key) -> bool {
        self.inner.insert(item)
    }
    fn remove(&mut self, key: &Key) -> bool {
        self.inner.remove(key)
    }
    fn contains(&self, key: &Key) -> bool {
        self.inner.contains(key)
    }
    fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }
    fn extend(&mut self, other: PeerSet<Key>) {
        self.inner.extend(other.inner)
    }
}

pub struct RouteNode<P, Key, Value> {
    children: Option<HashMap<RouteKey, RouteNode<P, Key, Value>>>,
    peers: P,
    _marker: PhantomData<(Key, Value)>
}

impl<P, Key, Value> RouteNode<P, Key, Value>
    where P: Peers<Key, Value>,
          Key: Eq + Hash
{
    pub fn new () -> RouteNode<P, Key, Value> {
        RouteNode {
            children: None,
            peers: P::new(),
            _marker: PhantomData
        }
    }

    pub fn insert(&mut self, tf: &TopicFilter, item: P::Item) -> bool {
        let mut last_node = (*tf).split("/").fold(self, |current, token| {
            let token = RouteKey::from(token);
            if current.children.is_none() {
                current.children = Some(HashMap::<RouteKey, RouteNode<P, Key, Value>>::new());
            }
            match current.children {
                Some(ref mut map) => {
                    if !map.contains_key(&token) {
                        map.insert(token.clone(), RouteNode::new());
                    }
                    map.get_mut(&token).unwrap()
                }
                _ => unreachable!()
            }
        });

        last_node.peers.insert(item)
    }

    pub fn is_empty(&self) -> bool {
        self.peers.is_empty() && (match self.children {
            Some(ref map) => map.is_empty(),
            None => true
        })
    }

    fn _remove<'a, Itoken>(&mut self, mut tokens: Itoken, key: &Key) -> bool
        where Itoken: Iterator<Item=&'a str>
    {
        match tokens.next() {
            Some(token) => {
                match self.children {
                    Some(ref mut map) => {
                        let mut child_empty = false;
                        let route_key = RouteKey::from(token);
                        let is_removed = match map.get_mut(&route_key) {
                            Some(node) => {
                                let is_removed = node._remove(tokens, key);
                                child_empty = node.is_empty();
                                is_removed
                            }
                            None => false
                        };
                        if child_empty {
                            map.remove(&route_key);
                        }
                        is_removed
                    }
                    None => false
                }
            }
            None => {
                self.peers.remove(key)
            }
        }
    }

    pub fn remove(&mut self, tf: &TopicFilter, key: &Key) -> bool {
        self._remove((*tf).split("/"), key)
    }

    fn _search(&self, tokens: &Vec<&str>, index: usize) -> P {
        let mut peers = P::new();
        if let Some(token) = tokens.get(index) {
            match self.children {
                Some(ref map) => {
                    if let Some(node) = map.get(&RouteKey::Normal(token.to_string())) {
                        peers.extend(node._search(tokens, index+1));
                    }
                    if let Some(node) = map.get(&RouteKey::OneLevel) {
                        peers.extend(node._search(tokens, index+1));
                    }
                    if let Some(node) = map.get(&RouteKey::RestLevels) {
                        peers.extend(node.peers.clone());
                    }
                }
                None => {}
            }
        } else {
            peers.extend(self.peers.clone());
        }
        peers
    }

    pub fn search(&self, name: &TopicName) -> P {
        self._search(&(*name).split("/").collect::<Vec<&str>>(), 0)
    }
}

pub struct Routes<P, Key, Value> {
    filter_routes: HashMap<UserId, RouteNode<P, Key, Value>>,
    name_routes: HashMap<UserId, HashMap<TopicName, P>>,
    topics: HashMap<UserId, HashMap<Key, HashSet<Topic>>>
}

impl<P, Key, Value> Routes<P, Key, Value>
    where P: Peers<Key, Value>,
          Key: Eq + Hash + Clone,
{
    pub fn new() -> Routes<P, Key, Value> {
        Routes {
            filter_routes: HashMap::<UserId, RouteNode<P, Key, Value>>::new(),
            name_routes: HashMap::<UserId, HashMap<TopicName, P>>::new(),
            topics: HashMap::<UserId, HashMap<Key, HashSet<Topic>>>::new()
        }
    }

    pub fn insert_topic(&mut self, user_id: UserId, topic: &Topic, key: &Key, item: P::Item) -> bool {
        if !self.filter_routes.contains_key(&user_id) {
            self.filter_routes.insert(user_id, RouteNode::new());
        }
        if !self.name_routes.contains_key(&user_id) {
            self.name_routes.insert(user_id, HashMap::<TopicName, P>::new());
        }
        if !self.topics.contains_key(&user_id) {
            self.topics.insert(user_id, HashMap::<Key, HashSet<Topic>>::new());
        }
        let mut filter_routes = self.filter_routes.get_mut(&user_id).unwrap();
        let mut name_routes = self.name_routes.get_mut(&user_id).unwrap();
        let mut topics = self.topics.get_mut(&user_id).unwrap();

        if !topics.contains_key(key) {
            topics.insert(key.clone(), HashSet::<Topic>::new());
        }
        topics.get_mut(key).unwrap()
            .insert(topic.clone());

        match topic {
            &Topic::Filter(ref topic_filter) => {
                filter_routes.insert(topic_filter, item)
            }
            &Topic::Name(ref topic_name) => {
                if !name_routes.contains_key(topic_name) {
                    name_routes.insert(topic_name.clone(), P::new());
                }
                name_routes.get_mut(topic_name).unwrap().insert(item)
            }
        }
    }

    pub fn remove_topic(&mut self, user_id: UserId, topic: &Topic, key: &Key) {
        if let Some(ref mut topics) = self.topics.get_mut(&user_id) {
            match topics.get_mut(key) {
                Some(ref mut topic_name_set) => topic_name_set.remove(topic),
                None => false
            };
        }
        match topic {
            &Topic::Filter(ref topic_filter) => {
                if let Some(ref mut filter_routes) = self.filter_routes.get_mut(&user_id) {
                    filter_routes.remove(topic_filter, key);
                }
            }
            &Topic::Name(ref topic_name) => {
                if let Some(ref mut name_routes) = self.name_routes.get_mut(&user_id) {
                    match name_routes.get_mut(topic_name) {
                        Some(ref mut client_map) => client_map.remove(key),
                        None => false
                    };
                }
            }
        }
    }

    pub fn remove_all_topics(&mut self, user_id: UserId, key: &Key) -> usize {
        if let Some(ref mut topics) = self.topics.get_mut(&user_id) {
            let removed_count = match topics.get_mut(key) {
                Some(ref mut topic_name_set) => {
                    let mut removed_count = 0;
                    for topic in topic_name_set.iter() {
                        removed_count += match topic {
                            &Topic::Filter(ref topic_filter) => {
                                if let Some(ref mut filter_routes) = self.filter_routes.get_mut(&user_id) {
                                    if filter_routes.remove(topic_filter, key) { 1 } else { 0 }
                                } else { 0 }
                            }
                            &Topic::Name(ref topic_name) => {
                                if let Some(ref mut name_routes) = self.name_routes.get_mut(&user_id) {
                                    match name_routes.get_mut(topic_name) {
                                        Some(ref mut client_map) => {
                                            if client_map.remove(key) { 1 } else { 0 }
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
            topics.remove(key);
            removed_count
        } else { 0 }
    }

    pub fn search(&self, user_id: UserId, topic_name: &TopicName) -> P {
        let mut peers = P::new();
        if let Some(ref name_routes) = self.name_routes.get(&user_id) {
            if let Some(client_map) = name_routes.get(topic_name) {
                peers.extend(client_map.clone());
            }
        }
        if let Some(ref filter_routes) = self.filter_routes.get(&user_id) {
            peers.extend(filter_routes.search(topic_name));
        }
        peers
    }
}
