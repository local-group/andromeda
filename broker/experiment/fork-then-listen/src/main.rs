

extern crate libc;
extern crate threadpool;
#[macro_use]
extern crate futures;
#[macro_use]
extern crate tokio_core;
extern crate net2;

use std::io;
use std::env;
use std::thread;
use std::net::SocketAddr;
// use std::sync::mpsc::{channel, Receiver, RecvError};

use net2::TcpBuilder;

// use threadpool::ThreadPool;
use futures::Sink;
use futures::sync::mpsc::{channel, Receiver};
use futures::{Future, Poll, Async};
use futures::stream::Stream;
use tokio_core::io::{copy, Io};
use tokio_core::net::{TcpListener, TcpStream};
use tokio_core::reactor::{Core};


fn getpid() -> libc::pid_t {
    let mut pid = 0;
    unsafe {
        pid = libc::getpid();
    }
    pid
}

fn fork(n: u32) -> libc::pid_t {
    let mut child_pid = 0;
    for i in 0..n {
        let current_pid;
        unsafe {
            child_pid = libc::fork();
            current_pid = libc::getpid();
        }
        println!("[Pid({})]: child={}, current={}", i, child_pid, current_pid);
        if child_pid == 0 {
            break;
        }
    }
    child_pid
}


struct Acceptor {
    rx: Receiver<(TcpStream, SocketAddr)>,
}

impl Acceptor {
    fn new(rx: Receiver<(TcpStream, SocketAddr)>) -> Acceptor {
        Acceptor{rx: rx}
    }
}

impl Stream for Acceptor {
    type Item = (TcpStream, SocketAddr);
    type Error = ();

    fn poll(&mut self) -> Poll<Option<(TcpStream, SocketAddr)>, ()> {
        self.rx.poll()
    }
}


/*
extern crate threadpool;
extern crate futures;
extern crate tokio_core;

use std::env;
use std::thread;
use std::net::SocketAddr;

use futures::Sink;
use futures::sync::mpsc::{channel, Receiver};
use futures::{Future};
use futures::stream::Stream;
use tokio_core::io::{copy, Io};
use tokio_core::net::{TcpListener, TcpStream};
use tokio_core::reactor::{Core};
 */

/*
fn start_loop(rx: Receiver<(TcpStream, SocketAddr)>) {
    let mut core = Core::new().unwrap();
    let handle = core.handle();
    let acceptor = Acceptor::new(rx);
    let done = acceptor.for_each(move |(socket, addr)| {
        println!("[Start for]: {:?}", addr);
        let (reader, writer) = socket.split();
        let amt = copy(reader, writer);

        let msg = amt.then(move |result| {
            match result {
                Ok(amt) => println!("wrote {} bytes to {}", amt, addr),
                Err(e) => println!("error on {}: {}", addr, e),
            }
            Ok(())
        });
        println!("[Spawn for]: {:?}", addr);
        handle.spawn(msg);
        Ok(())
    });
    core.run(done).unwrap();
}
*/


#[cfg(unix)]
fn reuse_port(builder: &TcpBuilder) -> io::Result<&TcpBuilder> {
    use net2::unix::UnixTcpBuilderExt;
    builder.reuse_port(true)
}

#[cfg(windows)]
fn reuse_port(builder: &TcpBuilder) -> io::Result<&TcpBuilder> {
    Ok(builder)
}

fn main() {
    let return_pid = fork(1);

    let addr = env::args().nth(1).unwrap_or("127.0.0.1:8080".to_string());
    let addr = addr.parse::<SocketAddr>().unwrap();

    let builder = TcpBuilder::new_v4()
        .unwrap_or_else(|err| panic!("Failed to create listener, {}", err));

    let builder = reuse_port(&builder)
        .and_then(|builder| builder.reuse_address(true))
        .and_then(|builder| builder.bind(addr))
        .unwrap_or_else(|err| panic!("Failed to bind {}, {}", addr, err));

    // let (tx, rx) = channel::<(TcpStream, SocketAddr)>(10);
    // thread::spawn(move || {
    //     start_loop(rx);
    // });

    let mut core = Core::new().unwrap();
    let handle = core.handle();

    // let socket = TcpListener::bind(&addr, &handle).unwrap();
    let socket = builder.listen(1024)
        .and_then(|l| TcpListener::from_listener(l, &addr, &handle))
        .unwrap_or_else(|err| panic!("Failed to listen, {}", err));
    println!("Listening on: {}", addr);

    let done = socket.incoming().for_each(move |(socket, addr)| {
        let (reader, writer) = socket.split();
        let amt = copy(reader, writer);

        let msg = amt.then(move |result| {
            match result {
                Ok(amt) => println!("wrote {} bytes to {}", amt, addr),
                Err(e) => println!("error on {}: {}", addr, e),
            }
            Ok(())
        });
        println!("[Spawn for(pid={})]: {:?}", getpid(), addr);
        handle.spawn(msg);
        Ok(())
        // let _ = tx.clone().send((socket, addr));
        // Ok(())
    });

    core.run(done).unwrap();
}
