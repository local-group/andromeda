use std::collections::HashMap;
use std::rc::Rc;
use std::cell::RefCell;
use std::iter;
use std::mem;
use std::thread;
use std::time;
use std::net::{SocketAddr, ToSocketAddrs};
use std::io::{self, Cursor, Error, ErrorKind, BufReader};
use std::sync::mpsc::{Sender};

use net2::TcpBuilder;
use bincode::{self, Infinite};
use bytes::BytesMut;
use bytes::buf::BufMut;
use byteorder::{BigEndian, ReadBytesExt};
use futures::{Future, Stream, Sink};
use futures::future::{BoxFuture};
use futures::stream::{self};
use futures::sync::mpsc;
use tokio_core::reactor::{Core, Handle};
use tokio_core::net::{TcpListener, TcpStream};
use tokio_io::{self, AsyncRead, AsyncWrite};
use tokio_io::codec::{Encoder, Decoder};
use tokio_tls::{TlsAcceptorExt, TlsConnectorExt};
use native_tls::{self, TlsAcceptor, TlsConnector};

use super::{CodecState, StoreRequest, StoreResponse};

#[cfg(unix)]
fn reuse_port(builder: &TcpBuilder) -> io::Result<&TcpBuilder> {
    use net2::unix::UnixTcpBuilderExt;
    builder.reuse_port(true)
}

#[cfg(windows)]
fn reuse_port(builder: &TcpBuilder) -> io::Result<&TcpBuilder> {
    Ok(builder)
}

fn native2io(e: native_tls::Error) -> io::Error {
    println!("native2io.error = {:?}", e);
    io::Error::new(io::ErrorKind::Other, e)
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

    fn handle_socket<S, Msg>(
        addr: SocketAddr,
        socket: S,
        tx: Sender<Msg>,
        handle: Handle,
        connections: Rc<RefCell<HashMap<SocketAddr, mpsc::UnboundedSender<Vec<u8>>>>>
    )
        -> Result<(), ()>
        where S: AsyncRead + AsyncWrite + 'static,
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
            let data = tokio_io::io::read(reader, vec![0; 2048]);
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
            tokio_io::io::write_all(writer, data)
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

    pub fn start_loop<M>(self, server_rx: mpsc::Receiver<ToNetMsg>, session_tx: Sender<M>)
        where M: MsgFromNet + 'static
    {
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
                        tx.send(data).wait().unwrap();
                    } else {
                        let m = M::disconnect(addr, "Connection closed unexpected!".to_owned());
                        cloned_session_tx.send(m).unwrap();
                    }
                }
                ToNetMsg::Disconnect(addr, _) => {
                    let mut connections = cloned_connections.borrow_mut();
                    if let Some(tx) = connections.get_mut(&addr) {
                        tx.send(Vec::new()).wait().unwrap();
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
                            Self::handle_socket(addr, socket, tx, cloned_handle, connections)
                        });
                    handle.spawn(accept);
                },
                None => {
                    let _ = Self::handle_socket(addr, socket, tx, cloned_handle, connections);
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


/*======================================== Cleint */

pub struct ClientCodec {
    state: CodecState
}

impl Encoder for ClientCodec {
    type Item = StoreRequest;
    type Error = io::Error;
    fn encode(&mut self, request: Self::Item, buf: &mut BytesMut) -> io::Result<()> {
        let data: Vec<u8> = bincode::serialize(&request, Infinite).unwrap();
        buf.put(data);
        Ok(())
    }
}

impl Decoder for ClientCodec {
    type Item = StoreResponse;
    type Error = io::Error;
    fn decode(&mut self, buf: &mut BytesMut) -> io::Result<Option<Self::Item>> {
        loop {
            match self.state {
                CodecState::Len if buf.len() < mem::size_of::<u64>() => {
                    return Ok(None);
                }
                CodecState::Len => {
                    let len_buf = buf.split_to(mem::size_of::<u64>());
                    let len = Cursor::new(len_buf).read_u64::<BigEndian>()?;
                    self.state = CodecState::Payload { len: len };
                }
                CodecState::Payload { len } if buf.len() < len as usize => {
                    return Ok(None);
                }
                CodecState::Payload { len } => {
                    let payload = buf.split_to(len as usize);
                    let result = bincode::deserialize_from(&mut Cursor::new(payload), Infinite).unwrap();
                    // Reset the state machine because, either way, we're done processing this
                    // message.
                    self.state = CodecState::Len;

                    return Ok(Some(result));
                }
            }
        }
    }
}

pub struct NetClient {
    addr: SocketAddr,
    tls_connector: Option<TlsConnector>,
}

impl NetClient {

    pub fn new(url: &str, tls_connector: Option<TlsConnector>) -> NetClient {
        let parts: Vec<&str> = url.split("://").collect();
        let _schema = parts[0];
        let server_port = parts[1];
        let addr = server_port.to_socket_addrs().unwrap().next().unwrap();
        NetClient {
            addr: addr,
            tls_connector: tls_connector
        }
    }

    fn handle_socket<S>(
        socket: S,
        tx: Sender<StoreResponse>,
        rx: mpsc::Receiver<StoreRequest>,
    ) -> BoxFuture<(), Error>
        where S: AsyncRead + AsyncWrite + Send + 'static
    {
        println!("Start socket");
        let codec = ClientCodec{ state: CodecState::Len };
        let (sink, stream) = socket.framed(codec).split();
        let send = rx
            .map_err(|_| Error::new(ErrorKind::InvalidData, "invalid data"))
            .forward(sink)
            .map_err(|_|());
        let tx = tx.clone();
        let recv = stream.map_err(|_|()).for_each(move |response| {
            tx.send(response).map_err(|_| ()).map(|_| ())
        });
        send.map(|_|())
            .select(recv.map(|_|()))
            .then(|_| Ok(()))
            .boxed()
    }

    pub fn start_loop(self, client_rx: mpsc::Receiver<StoreRequest>,
                      outter_tx: Sender<StoreResponse>) {
        let retry = 3;
        let mut core = Core::new().unwrap();
        let socket = TcpStream::connect(&self.addr, &core.handle());
        match self.tls_connector {
            Some(ref tls_connector) => {
                let network = socket.and_then(|socket| {
                    tls_connector.connect_async("localhost", socket).map_err(native2io)
                }).and_then(move |socket| {
                    NetClient::handle_socket(socket, outter_tx, client_rx)
                });
                let network = network.map_err(|e| {
                    println!("future error(network): error={:?}, client={}",
                             e, &self.addr);
                    ()
                });
                let done = network.shared();
                for n in 0..retry {
                    match core.run(done.clone()) {
                        Ok(_) => {
                            println!("Core run OK!!!");
                            break;
                        },
                        Err(e) => {
                            println!("Run core ERROR: error={:?}, retry={}, client={}",
                                     e, n, &self.addr);
                            thread::sleep(time::Duration::from_millis(20));
                        }
                    }
                }
            },
            None => {
                let network = socket.and_then(move |socket| {
                    NetClient::handle_socket(socket, outter_tx, client_rx)
                });
                let network = network.map_err(|e| {
                    println!("future error(network): error={:?}, client={}",
                             e, &self.addr);
                    ()
                });
                let done = network.shared();
                for n in 0..retry {
                    match core.run(done.clone()) {
                        Ok(_) => {
                            println!("Core run OK!!!");
                            break;
                        },
                        Err(e) => {
                            println!("Run core ERROR: error={:?}, retry={}, client={}",
                                     e, n, &self.addr);
                            thread::sleep(time::Duration::from_millis(20));
                        }
                    }
                }
            }
        }
    }
}
