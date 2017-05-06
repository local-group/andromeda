
use std::collections::HashMap;
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

use super::{ClientConnectionMsg, ClientSessionMsg};

pub fn run_epoll(
    addr: SocketAddr, tls_acceptor: Option<TlsAcceptor>,
    client_connection_rx: mpsc::Receiver<ClientConnectionMsg>,
    client_session_tx: Sender<ClientSessionMsg>,
) {
    let client_connection_rx = client_connection_rx.map_err(|_| panic!());

    let builder = TcpBuilder::new_v4()
        .unwrap_or_else(|err| panic!("Failed to create listener, {}", err));
    let builder = reuse_port(&builder)
        .and_then(|builder| builder.reuse_address(true))
        .and_then(|builder| builder.bind(addr))
        .unwrap_or_else(|err| panic!("Failed to bind {}, {}", addr, err));

    let mut core = Core::new().unwrap();
    let handle = core.handle();

    let socket = builder.listen(1024)
        .and_then(|l| TcpListener::from_listener(l, &addr, &handle))
        .unwrap_or_else(|err| panic!("Failed to listen, {}", err));

    debug!("Listenering on: {}", addr);

    let connections = Rc::new(RefCell::new(
        HashMap::<SocketAddr, mpsc::UnboundedSender<Vec<u8>>>::new()
    ));

    // Forward data from session to connection.
    let cloned_connections = connections.clone();
    let cloned_client_session_tx = client_session_tx.clone();
    let inbox = client_connection_rx.for_each(move |msg| {
        debug!("> inbox.msg = {:?}", msg);
        match msg {
            ClientConnectionMsg::Data(addr, data) => {
                let mut connections = cloned_connections.borrow_mut();
                if let Some(tx) = connections.get_mut(&addr) {
                    tx.send(data).unwrap();
                } else {
                    let m = ClientSessionMsg::ClientDisconnect(addr, "Connection closed unexpected!".to_owned());
                    cloned_client_session_tx.send(m).unwrap();
                }
            }
            ClientConnectionMsg::DisconnectClient(addr, _) => {
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
        let client_session_tx = client_session_tx.clone();

        match tls_acceptor {
            Some(ref tls_acceptor) => {
                let accept = tls_acceptor
                    .accept_async(socket)
                    .map_err(|e| {
                        debug!("> accept_async.err = {:?}", e);
                        ()
                    })
                    .and_then(move |socket| {
                        handle_socket(addr, socket, client_session_tx, cloned_handle, connections)
                    });
                handle.spawn(accept);
            },
            None => {
                let _ = handle_socket(addr, socket, client_session_tx, cloned_handle, connections);
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


fn handle_socket<S>(
    addr: SocketAddr,
    socket: S,
    client_session_tx: Sender<ClientSessionMsg>,
    handle: Handle,
    connections: Rc<RefCell<HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>>>>
)
    -> Result<(), ()>
    where S: Io + 'static
{
    debug!("> socket.incoming().addr = {:?}", addr);
    let (reader, writer) = socket.split();

    let connections = connections.clone();
    let (tx, rx) = mpsc::unbounded::<Vec<u8>>();
    let mut conns = connections.borrow_mut();
    conns.insert(addr, tx);

    let reader = BufReader::new(reader);

    // Forward received socket data to `client_session`
    let cloned_client_session_tx = client_session_tx.clone();
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
        let cloned_client_session_tx = cloned_client_session_tx.clone();
        data.map(move |(reader, buf, n)| {
            let buf = buf.iter().take(n).cloned().collect();
            debug!("> [server] Received: {:?}", buf);
            let msg = ClientSessionMsg::Data(addr, buf);
            cloned_client_session_tx.send(msg).unwrap();
            reader
        })
    });

    // Receive data from `inbox` then write to socket
    let socket_writer = rx.fold(writer, |writer, data| {
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

    let cloned_client_session_tx = client_session_tx.clone();
    let connections = connections.clone();
    handle.spawn(connection.then(move |_| {
        let m = ClientSessionMsg::ClientDisconnect(addr, "Normal disconnect!".to_owned());
        cloned_client_session_tx.send(m).unwrap();
        connections.borrow_mut().remove(&addr);
        debug!("Connection {} closed.", addr);
        Ok(())
    }));
    Ok(())
}

#[cfg(unix)]
fn reuse_port(builder: &TcpBuilder) -> io::Result<&TcpBuilder> {
    use net2::unix::UnixTcpBuilderExt;
    builder.reuse_port(true)
}

#[cfg(windows)]
fn reuse_port(builder: &TcpBuilder) -> io::Result<&TcpBuilder> {
    Ok(builder)
}

