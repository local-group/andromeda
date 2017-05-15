
use std::mem;
use std::io::{Cursor};
use std::net::{SocketAddr};
use std::collections::{HashMap};
use std::sync::mpsc::{Sender, Receiver};

use bincode::{self, Infinite};
use byteorder::{BigEndian, ReadBytesExt};
use bytes::BytesMut;
use bytes::buf::BufMut;
use tokio_core::io::{EasyBuf};
use futures::{Sink, Future};
use futures::sync::mpsc;

use super::{StoreSessionMsg, GlobalRouterMsg};
use common::{ToNetMsg, StoreRequest, StoreResponse};

pub fn run(
    session_rx: Receiver<StoreSessionMsg>,
    server_tx: mpsc::Sender<ToNetMsg>,
    router_tx: Sender<GlobalRouterMsg>,
) {
    let mut sessions = HashMap::<SocketAddr, Session>::new();
    loop {
        match session_rx.recv().unwrap() {
            StoreSessionMsg::Data(addr, data) => {
                if !sessions.contains_key(&addr) {
                    sessions.insert(addr, Session::new(
                        addr, server_tx.clone(), router_tx.clone()
                    ));
                }
                let mut session = sessions.get_mut(&addr).unwrap();
                session.decode(data);
            }
            StoreSessionMsg::Disconnect(addr, msg) => {
                // TODO:
            }
            StoreSessionMsg::Publish(addr, user_id, packet) => {
                if let Some(session) = sessions.get_mut(&addr) {
                    session.handle_response(StoreResponse::Publish(user_id, packet));
                } else {
                    warn!("[StoreSessionMsg::Publish]: no session: addr={:?}, user_id={:?}", addr, user_id);
                }
            }
            StoreSessionMsg::Retains(addr, user_id, client_identifier, packets, qos) => {
                if let Some(session) = sessions.get_mut(&addr) {
                    session.handle_response(StoreResponse::Retains(user_id, client_identifier, packets, qos));
                } else {
                    warn!("[StoreSessionMsg::Retains] no session: addr={:?}, user_id={:?}, client_identifier={:?}",
                          addr, user_id, client_identifier);
                }
            }
        };
    }
}


pub struct Session {
    addr: SocketAddr,
    // Data length
    header: Option<u64>,
    buf: BytesMut,
    server_tx: mpsc::Sender<ToNetMsg>,
    router_tx: Sender<GlobalRouterMsg>,
}

impl Session {
    fn new(
        addr: SocketAddr,
        server_tx: mpsc::Sender<ToNetMsg>,
        router_tx: Sender<GlobalRouterMsg>
    ) -> Session {
        Session {
            addr: addr,
            buf: BytesMut::with_capacity(2048),
            header: None,
            server_tx: server_tx,
            router_tx: router_tx
        }
    }

    fn decode(&mut self, data: Vec<u8>) {
        self.buf.put(data);
        loop {
            if self.header.is_none() {
                if self.buf.len() >= mem::size_of::<u64>() {
                    let len_buf = self.buf.split_to(mem::size_of::<u64>());
                    let len = Cursor::new(len_buf).read_u64::<BigEndian>().unwrap();
                    self.header = Some(len);
                } else {
                    // Need more bytes to decode header
                    break;
                }
            } else {
                let length = self.header.unwrap() as usize;
                assert_eq!(length > 0 , true);
                if self.buf.len() >= length {
                    let payload = self.buf.split_to(length);
                    let request: StoreRequest = bincode::deserialize(&payload[..]).unwrap();
                    self.handle_request(request);
                } else {
                    // Need more bytes to decode payload
                    break;
                }
            }
        }
    }

    fn handle_request(&self, request: StoreRequest) {
        match request {
            StoreRequest::Publish(user_id, packet) => {
                let msg = GlobalRouterMsg::Publish(self.addr, user_id, packet);
                self.router_tx.send(msg).unwrap();
            }
            StoreRequest::Subscribe(user_id, topics) => {
                let msg = GlobalRouterMsg::Subscribe(self.addr, user_id, topics);
                self.router_tx.send(msg).unwrap();
            }
            StoreRequest::Unsubscribe(user_id, topics) => {
                let msg = GlobalRouterMsg::Unsubscribe(self.addr, user_id, topics);
                self.router_tx.send(msg).unwrap();
            }
            StoreRequest::GetRetains(user_id, client_identifier, topic, subscribe_qos) => {
                // TODO
            }
        }
    }

    fn handle_response(&self, response: StoreResponse) {
        let data: Vec<u8> = bincode::serialize(&response, Infinite).unwrap();
        let msg = ToNetMsg::Data(self.addr, data);
        self.server_tx.clone().send(msg).wait().unwrap();
    }
}
