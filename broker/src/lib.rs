#[macro_use]
extern crate log;
extern crate bytes;
extern crate byteorder;
extern crate bincode;
#[macro_use]
extern crate serde_derive;
extern crate serde;
extern crate env_logger;
extern crate mqtt;
extern crate threadpool;
extern crate net2;
extern crate mio;
extern crate futures;
extern crate tokio_core;
extern crate native_tls;
extern crate tokio_tls;

pub mod common;
pub mod hub;
pub mod store;

use std::net::{SocketAddr};
use std::thread;
use std::sync::mpsc::{channel};

use futures::sync::mpsc;
use native_tls::{TlsAcceptor};



pub fn run_simple(addr: SocketAddr, tls_acceptor: Option<TlsAcceptor>) {
    let (client_connection_tx, client_connection_rx) = mpsc::channel::<hub::ClientConnectionMsg>(1024);
    let (client_session_tx, client_session_rx) = channel::<hub::ClientSessionMsg>();
    let (session_timer_tx, session_timer_rx) = channel::<(hub::SessionTimerAction, hub::SessionTimerPayload)>();
    let (local_router_tx, local_router_rx) = channel::<hub::LocalRouterMsg>();
    let (router_follower_tx, router_follower_rx) = mpsc::channel::<hub::RouterFollowerMsg>(1024);
    let (router_leader_tx, router_leader_rx) = mpsc::channel::<hub::RouterLeaderMsg>(1024);
    let (global_retain_tx, global_retain_rx) = channel::<store::GlobalRetainMsg>();

    let router_leader = thread::spawn(move || {
        hub::router_leader::run_epoll(router_leader_rx);
    });

    let cloned_local_router_tx = local_router_tx.clone();
    let router_follower = thread::spawn(move || {
        hub::router_follower::run_epoll(router_follower_rx, cloned_local_router_tx);
    });

    let cloned_client_session_tx = client_session_tx.clone();
    let session_timer = thread::spawn(move || {
        hub::session_timer::run(session_timer_rx, cloned_client_session_tx);
    });

    let cloned_local_router_tx = local_router_tx.clone();
    let cloned_client_session_tx = client_session_tx.clone();
    let cloned_router_follower_tx = router_follower_tx.clone();
    let cloned_router_leader_tx = router_leader_tx.clone();
    let cloned_global_retain_tx = global_retain_tx.clone();
    let local_router = thread::spawn(move || {
        hub::local_router::run(local_router_rx,
                               cloned_local_router_tx,
                               cloned_client_session_tx,
                               cloned_router_follower_tx,
                               cloned_router_leader_tx,
                               cloned_global_retain_tx);
    });

    let cloned_client_connection_tx = client_connection_tx.clone();
    let cloned_local_router_tx = local_router_tx.clone();
    let cloned_global_retain_tx = global_retain_tx.clone();
    let client_session = thread::spawn(move || {
        hub::client_session::run(client_session_rx,
                                 session_timer_tx,
                                 cloned_client_connection_tx,
                                 cloned_local_router_tx,
                                 cloned_global_retain_tx);
    });

    let cloned_client_session_tx = client_session_tx.clone();
    let client_connection = thread::spawn(move || {
        hub::client_connection::run_epoll(addr, tls_acceptor,
                                          client_connection_rx,
                                          cloned_client_session_tx);
    });

    let cloned_client_session_tx = client_session_tx.clone();
    let global_retain = thread::spawn(move || {
        store::global_retain::run(global_retain_rx, cloned_client_session_tx);
    });

    for handle in vec![client_connection, client_session, session_timer,
                       local_router,
                       router_follower, router_leader,
                       global_retain] {
        handle.join().unwrap();
    }
}
