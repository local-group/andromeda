use std::collections::HashMap;
use std::fmt::Debug;
use std::rc::Rc;
use std::cell::RefCell;
use std::iter;
use std::net::SocketAddr;
use std::io::{self, Error, ErrorKind, BufReader};
use std::sync::mpsc::{Sender};

use net2::TcpBuilder;
use futures::{Future, Stream};
use futures::stream::{self};
use futures::sync::mpsc;
use tokio_core::reactor::{Core, Handle};
use tokio_core::io::{self as tokio_io, Io};
use tokio_core::net::{TcpListener};
use tokio_tls::{TlsAcceptorExt};
use native_tls::{TlsAcceptor};

#[cfg(unix)]
fn reuse_port(builder: &TcpBuilder) -> io::Result<&TcpBuilder> {
    use net2::unix::UnixTcpBuilderExt;
    builder.reuse_port(true)
}

#[cfg(windows)]
fn reuse_port(builder: &TcpBuilder) -> io::Result<&TcpBuilder> {
    Ok(builder)
}

pub trait MsgFromNet {
    fn data(addr: SocketAddr, buf: Vec<u8>) -> Self;
    fn disconnect(addr: SocketAddr, msg: String) -> Self;
}

#[derive(Debug, Clone)]
pub enum ToNetMsg {
    Data(SocketAddr, Vec<u8>),
    Disconnect(SocketAddr, String)
}

fn handle_socket<S, Msg>(
    addr: SocketAddr,
    socket: S,
    tx: Sender<Msg>,
    handle: Handle,
    connections: Rc<RefCell<HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>>>>
)
    -> Result<(), ()>
    where S: Io + 'static,
          Msg: MsgFromNet + 'static
{
    debug!("> socket.incoming().addr = {:?}", addr);
    let (reader, writer) = socket.split();

    let connections = connections.clone();
    let (inner_tx, inner_rx) = mpsc::unbounded::<Vec<u8>>();
    let mut conns = connections.borrow_mut();
    conns.insert(addr, inner_tx);

    let reader = BufReader::new(reader);

    // Forward received socket data to `client_session`
    let cloned_tx = tx.clone();
    let iter = stream::iter(iter::repeat(()).map(Ok::<(), Error>));
    let socket_reader = iter.fold(reader, move |reader, _| {
        // TODO: read_exact(reader, [u8; 1]), the performance is really bad!!!
        let data = tokio_io::read(reader, [0; 32]);
        let data = data.and_then(|(reader, buf, n)| {
            // debug!("> Read {} bytes({:?}) from client", buf.len(), buf);
            if n == 0 {
                Err(Error::new(ErrorKind::BrokenPipe, "broken pipe"))
            } else {
                Ok((reader, buf, n))
            }
        });
        let cloned_tx = cloned_tx.clone();
        data.map(move |(reader, buf, n)| {
            let buf = buf.iter().take(n).cloned().collect();
            debug!("> [server] Received: {:?}", buf);
            let msg = Msg::data(addr, buf);
            cloned_tx.send(msg).unwrap();
            reader
        })
    });

    // Receive data from `inbox` then write to socket
    let socket_writer = inner_rx.fold(writer, |writer, data| {
        debug!("> [server] Write all: {:?}", data);
        tokio_io::write_all(writer, data)
            .and_then(|(writer, data)| {
                if data.is_empty() {
                    // TODO:: need more general fix.
                    Err(Error::new(ErrorKind::BrokenPipe, "quit"))
                } else {
                    Ok((writer, data))
                }
            })
            .map(|(writer, _)| writer)
            .map_err(|e| {
                debug!("> socket_writer.err = {:?}", e);
                ()
            })
    });

    let socket_reader = socket_reader.map_err(|e| {
        debug!("> socket_reader.err = {:?}", e);
        ()
    });
    // ReadHalf finished or WriteHalf finished then close the socket
    let connection = socket_reader.map(|_| ()).select(socket_writer.map(|_| ()));

    let cloned_tx = tx.clone();
    let connections = connections.clone();
    handle.spawn(connection.then(move |_| {
        let m = Msg::disconnect(addr, "Normal disconnect!".to_owned());
        cloned_tx.send(m).unwrap();
        connections.borrow_mut().remove(&addr);
        debug!("Connection {} closed.", addr);
        Ok(())
    }));
    Ok(())
}

pub struct NetServer {
    addr: SocketAddr,
    tls_acceptor: Option<TlsAcceptor>,
}

impl NetServer {
    pub fn new(addr: SocketAddr, tls_acceptor: Option<TlsAcceptor>) -> NetServer {
        NetServer{
            addr: addr,
            tls_acceptor: tls_acceptor,
        }
    }

    pub fn start_loop<M>(self, server_rx: mpsc::Receiver<ToNetMsg>, session_tx: Sender<M>)
        where M: MsgFromNet + 'static
    {
        let addr = self.addr;
        let server_rx = server_rx.map_err(|_| panic!());

        let builder = TcpBuilder::new_v4()
            .unwrap_or_else(|err| panic!("Failed to create listener, {}", err));
        let builder = reuse_port(&builder)
            .and_then(|builder| builder.reuse_address(true))
            .and_then(|builder| builder.bind(self.addr))
            .unwrap_or_else(|err| panic!("Failed to bind {}, {}", self.addr, err));

        let mut core = Core::new().unwrap();
        let handle = core.handle();

        let socket = builder.listen(1024)
            .and_then(|l| TcpListener::from_listener(l, &self.addr, &handle))
            .unwrap_or_else(|err| panic!("Failed to listen, {}", err));

        debug!("Listenering on: {}", self.addr);

        let connections = Rc::new(RefCell::new(
            HashMap::<SocketAddr, mpsc::UnboundedSender<Vec<u8>>>::new()
        ));

        // Forward data from session to connection.
        let cloned_connections = connections.clone();
        let cloned_session_tx = session_tx.clone();
        let inbox = server_rx.for_each(move |msg| {
            debug!("> inbox.msg = {:?}", msg);
            match msg {
                ToNetMsg::Data(addr, data) => {
                    let mut connections = cloned_connections.borrow_mut();
                    if let Some(tx) = connections.get_mut(&addr) {
                        tx.send(data).unwrap();
                    } else {
                        let m = M::disconnect(addr, "Connection closed unexpected!".to_owned());
                        cloned_session_tx.send(m).unwrap();
                    }
                }
                ToNetMsg::Disconnect(addr, _) => {
                    let mut connections = cloned_connections.borrow_mut();
                    if let Some(tx) = connections.get_mut(&addr) {
                        tx.send(Vec::new()).unwrap();
                    }
                }
            };
            Ok(())
        });

        let serv = socket.incoming().for_each(move |(socket, addr)| {
            let cloned_handle = handle.clone();
            let connections = connections.clone();
            let tx = session_tx.clone();

            match self.tls_acceptor {
                Some(ref tls_acceptor) => {
                    let accept = tls_acceptor
                        .accept_async(socket)
                        .map_err(|e| {
                            debug!("> accept_async.err = {:?}", e);
                            ()
                        })
                        .and_then(move |socket| {
                            handle_socket(addr, socket, tx, cloned_handle, connections)
                        });
                    handle.spawn(accept);
                },
                None => {
                    let _ = handle_socket(addr, socket, tx, cloned_handle, connections);
                }
            };
            Ok(())
        });

        let inbox = inbox.map_err(|e| {
            debug!("> inbox.err = {:?}", e);
            ()
        });
        let serv = serv.map_err(|e| {
            debug!("> serv.err = {:?}", e);
            ()
        });
        let done = inbox.map(|_| ()).join(serv.map(|_| ()));
        core.run(done).unwrap();
    }
}
