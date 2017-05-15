
use std::net::{SocketAddr, IpAddr, Ipv4Addr};
use std::collections::{HashMap};
use std::sync::mpsc::{Receiver, Sender};

use mqtt::packet::{PublishPacket};

use common::{Topic, UserId, ClientIdentifier};
use hub;
use super::{GlobalRetainMsg};

pub fn run(
    global_retain_rx: Receiver<GlobalRetainMsg>,
    client_session_tx: Sender<hub::ClientSessionMsg>
) {
    let mut retains = HashMap::<UserId, RetainNode>::new();
    loop {
        let msg = global_retain_rx.recv().unwrap();
        match msg {
            GlobalRetainMsg::Insert(user_id, packet) => {
                if !retains.contains_key(&user_id) {
                    retains.insert(user_id, RetainNode::new());
                }
                let node = retains.get_mut(&user_id).unwrap();
                node.insert(&packet);
            }
            GlobalRetainMsg::Remove(user_id, topic_name) => {
                if let Some(ref mut node) = retains.get_mut(&user_id) {
                    node.remove(&topic_name);
                }
            }
            GlobalRetainMsg::MatchAll(user_id, addr, topic, qos) => {
                if let Some(ref node) = retains.get(&user_id) {
                    let packets = node.match_all(&topic);
                    // FIXME: addr, client_identifier
                    let msg = hub::ClientSessionMsg::RetainPackets(user_id, addr, packets, qos);
                    client_session_tx.send(msg).unwrap();
                }
            }
        };
    }
}

#[derive(Debug)]
pub struct RetainNode {
    children: Option<HashMap<String, RetainNode>>,
    packet: Option<PublishPacket>
}


impl RetainNode {

    pub fn new() -> RetainNode {
        RetainNode {
            children: None,
            packet: None
        }
    }

    pub fn insert(&mut self, packet: &PublishPacket) {
        let mut last_node = packet.topic_name().split("/").fold(self, |current, token| {
            let token = token.to_string();
            if current.children.is_none() {
                current.children = Some(HashMap::<String, RetainNode>::new());
            }
            match current.children {
                Some(ref mut map) => {
                    if !map.contains_key(&token) {
                        map.insert(token.clone(), RetainNode::new());
                    }
                    map.get_mut(&token).unwrap()
                }
                None => unreachable!()
            }
        });
        last_node.packet = Some(packet.clone());
    }

    pub fn is_empty(&self) -> bool {
        (match self.children {
            Some(ref map) => map.is_empty(),
            None => true
        }) && self.packet.is_none()
    }

    fn _remove<'a, I>(&mut self, mut tokens: I) -> bool
        where I: Iterator<Item=&'a str>
    {
        match tokens.next() {
            Some(token) => {
                match self.children {
                    Some(ref mut map) => {
                        let mut child_empty = false;
                        let is_removed = match map.get_mut(token) {
                            Some(node) => {
                                let is_removed = node._remove(tokens);
                                child_empty = node.is_empty();
                                is_removed
                            }
                            None => false
                        };
                        if child_empty {
                            map.remove(token);
                        }
                        is_removed
                    }
                    None => false
                }
            }
            None => {
                let is_removed = self.packet.is_some();
                self.packet = None;
                is_removed
            }
        }
    }

    pub fn remove(&mut self, topic_name: &str) -> bool {
        self._remove(topic_name.split("/"))
    }

    fn _match_all(&self, tokens: &Vec<&str>, index: usize) -> Vec<PublishPacket> {
        let mut packets: Vec<PublishPacket> = Vec::new();
        if let Some(token) = tokens.get(index) {
            if let Some(ref map) = self.children {
                match *token {
                    "#" => {
                        for (_, node) in map {
                            packets.extend(node._match_all(&vec!["#"], 0));
                            if let Some(ref packet) = node.packet {
                                packets.push(packet.clone());
                            }
                        }
                    }
                    "+" => {
                        for (_, node) in map {
                            packets.extend(node._match_all(tokens, index+1));
                        }
                    }
                    token @ _ => {
                        if let Some(node) = map.get(token) {
                            packets.extend(node._match_all(tokens, index+1));
                        }
                    }
                }
            }
        } else {
            if let Some(ref packet) = self.packet {
                packets.push(packet.clone());
            }
        }
        packets
    }

    pub fn match_all(&self, topic: &Topic) -> Vec<PublishPacket> {
        let tokens: Vec<&str> = (match topic {
            &Topic::Filter(ref filter) => (*filter).split("/"),
            &Topic::Name(ref name) => (*name).split("/")
        }).collect::<Vec<&str>>();
        self._match_all(&tokens, 0)
    }
}
