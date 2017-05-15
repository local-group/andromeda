
pub mod global_router;
pub mod global_retain;
pub mod store_session;

use std::thread;
use std::sync::mpsc::{Sender, Receiver, channel};
use std::net::SocketAddr;

use mqtt::{QualityOfService};
use mqtt::packet::{PublishPacket};
// Global session storage

use native_tls::{TlsAcceptor};
use futures::sync::mpsc;

use common::{UserId, ClientIdentifier, Topic,
             MsgFromConnection, ToConnectionMsg, ConnectionMgr};

#[derive(Debug, Clone, Hash, Eq, PartialEq)]
pub struct HubNode(pub SocketAddr);

// Use like: (HubNode, StoreSessionMsg)
/// [router, retain] => session
#[derive(Debug, Clone)]
pub enum StoreSessionMsg {
    Data(SocketAddr, Vec<u8>),
    Disconnect(SocketAddr, String),
    Publish(SocketAddr, UserId, PublishPacket),
    /// qos is subscribe qos
    Retains(SocketAddr, UserId, ClientIdentifier, Vec<PublishPacket>, QualityOfService)
}

impl MsgFromConnection for StoreSessionMsg {
    fn data(addr: SocketAddr, buf: Vec<u8>) -> StoreSessionMsg {
        StoreSessionMsg::Data(addr, buf)
    }
    fn disconnect(addr: SocketAddr, msg: String) -> StoreSessionMsg {
        StoreSessionMsg::Disconnect(addr, msg)
    }
}

// Used as: (HubNode, GlobalRouterMsg)
/// session => router
#[derive(Debug, Clone)]
pub enum GlobalRouterMsg {
    Publish(SocketAddr, UserId, PublishPacket),
    Subscribe(SocketAddr, UserId, Vec<Topic>),
    Unsubscribe(SocketAddr, UserId, Vec<Topic>),
    NodeDisconnect(SocketAddr)
}

// Used as: (HubNode, GlobalRetainMsg)
/// session => retain
#[derive(Debug, Clone)]
pub enum GlobalRetainMsg {
    Insert(UserId, PublishPacket),
    Remove(UserId, String),
    MatchAll(UserId, SocketAddr, Topic, QualityOfService)
}

pub fn run(addr: SocketAddr, tls_acceptor: Option<TlsAcceptor>) {
    let (session_tx, session_rx) = channel::<StoreSessionMsg>();
    let (router_tx, router_rx) = channel::<GlobalRouterMsg>();
    let (conn_tx, conn_rx) = mpsc::channel::<ToConnectionMsg>(1024);
    // Session
    let cloned_conn_tx = conn_tx.clone();
    let cloned_router_tx = router_tx.clone();
    let session_thread = thread::spawn(move || {
        store_session::run(session_rx, cloned_conn_tx, cloned_router_tx);
    });
    // Router
    let cloned_session_tx = session_tx.clone();
    let router_thread = thread::spawn(move || {
        global_router::run_epoll(router_rx, cloned_session_tx);
    });
    // Connection Manager
    let conn_mgr = ConnectionMgr::new(addr, tls_acceptor);
    let cloned_session_tx = session_tx.clone();
    let conn_thread = thread::spawn(move || {
        conn_mgr.start_loop(conn_rx, cloned_session_tx);
    });
    for handle in vec![session_thread, router_thread, conn_thread] {
        handle.join().unwrap();
    }
}
