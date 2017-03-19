extern crate mqtt;
extern crate futures;
extern crate native_tls;
extern crate tokio_core;
extern crate tokio_tls;

use std::time;
use std::iter;
use std::thread;
use std::io::{self, Error, ErrorKind, BufReader};
// use std::sync::mpsc::{channel, Receiver};
use std::net::{ToSocketAddrs, SocketAddr};

use futures::{Future, Stream, Sink};
use futures::future::{self, BoxFuture};
use futures::stream;
use futures::sync::{mpsc, oneshot};
use native_tls::TlsConnector;
use tokio_core::net::TcpStream;
use tokio_core::reactor::Core;
use tokio_core::io::{self as tokio_io, Io, EasyBuf};
use tokio_tls::TlsConnectorExt;

use mqtt::{Encodable, Decodable, TopicFilter, TopicName, QualityOfService};
use mqtt::packet::{QoSWithPacketIdentifier, VariablePacket,
                   ConnectPacket, // PingreqPacket,
                   SubscribePacket, UnsubscribePacket, PublishPacket};
use mqtt::control::{FixedHeader};

fn native2io(e: native_tls::Error) -> io::Error {
    println!("native2io.error = {:?}", e);
    io::Error::new(io::ErrorKind::Other, e)
}

// #[derive(Debug)]
pub enum ClientMsg {
    Data(Vec<u8>),
    Shutdown(oneshot::Sender<()>)
}

pub struct Client {
    addr: SocketAddr,
    // addr_str: String,
    tls_connector: Option<TlsConnector>,
    keep_alive: u32,
    client_identifier: String,
    clean_session: bool,
    ignore_pingresp: bool,
    will: Option<(TopicName, Vec<u8>)>,
    will_qos: Option<u8>,
    will_retain: Option<bool>,
    pkid: u16,
    // started: bool,
    inbox: Option<mpsc::Receiver<VariablePacket>>,
    postman: Option<mpsc::Sender<Vec<u8>>>,
    recv_inbox: Option<mpsc::Sender<ClientMsg>>,
}


impl Client {
    pub fn new(url: &str,       // ssl://140.205.202.2:8080
               tls_connector: Option<TlsConnector>,
               keep_alive: u32,
               client_identifier: &str, clean_session: bool,
               ignore_pingresp: bool,
               will: Option<(&str, Vec<u8>)>,
               will_qos: Option<u8>, will_retain: Option<bool>) -> Client {
        let will = will.map(|(topic, message)| {
            (TopicName::new(topic.to_string()).unwrap(), message)
        });
        let parts: Vec<&str> = url.split("://").collect();
        let _schema = parts[0];
        let server_port = parts[1];
        let addr = server_port.to_socket_addrs().unwrap().next().unwrap();

        Client {
            addr: addr,
            // addr_str: server_port.to_owned(),
            tls_connector: tls_connector,
            keep_alive: keep_alive,
            client_identifier: client_identifier.to_string(),
            clean_session: clean_session,
            ignore_pingresp: ignore_pingresp,
            will: will,
            will_qos: will_qos,
            will_retain: will_retain,
            pkid: 0,
            // started: false,
            inbox: None,
            postman: None,
            recv_inbox: None
        }
    }

    pub fn connect<F>(&mut self, retry: usize, handler: F)
        where F: FnOnce(&mut Client) -> (),
              F: Send + 'static,
    {
        let client_identifier = self.client_identifier.clone();
        let ignore_pingresp = self.ignore_pingresp;
        let addr = self.addr.clone();
        let tls_connector = self.tls_connector.take();
        let (tx, rx) = mpsc::channel::<Vec<u8>>(2);
        self.postman = Some(tx);
        let (inbox_sender, inbox) = mpsc::channel::<VariablePacket>(2);
        let (recv_tx, recv_rx) = mpsc::channel::<ClientMsg>(2);

        println!("Starting event loop...");
        thread::spawn(move || {
            let mut core = Core::new().unwrap();
            let mut buf = EasyBuf::new();
            let mut fixed_header: Option<FixedHeader> = None;
            let recv_packets = recv_rx.for_each(move |msg| {
                // println!("[client] Receive msg: {:?}", msg);
                match msg {
                    ClientMsg::Data(data) => {
                        buf.get_mut().extend_from_slice(data.as_slice());
                        if fixed_header.is_none() {
                            let header_len = {
                                // Valid fixed header length: [1, 5]
                                let buf_slice = buf.as_slice();
                                // [Values]:
                                //   None    => No enough bytes
                                //   Some(6) => Error fixed header
                                let mut rv: Option<usize> = None;
                                for i in 1..6 {
                                    if buf_slice.len() < i+1 {
                                        break
                                    }
                                    if buf_slice[i] & 0x80 == 0 {
                                        rv = Some(i + 1);
                                        break
                                    }
                                }
                                rv
                            };
                            if let Some(header_len) = header_len {
                                if header_len >= 6 {
                                    // Error fixed header, close Client!
                                    // let msg = ClientConnectionMsg::DisconnectClient(addr);
                                    // self.client_connection_tx.clone().send(msg).wait().unwrap();
                                }
                                // Decode packet fixed_header
                                let header_buf = buf.drain_to(header_len);
                                let mut bytes = header_buf.as_ref();
                                println!("Decoding fixed header");
                                fixed_header = Some(FixedHeader::decode_with(&mut bytes, None).unwrap());
                                println!("Decoded fixed header: {:?}", fixed_header);
                            } else {
                                // debug!("Not enough bytes to decode FixedHeader: length={}", self.buf.len());
                            }
                        }

                        match fixed_header {
                            Some(header) => {
                                let remaining_length = header.remaining_length as usize;
                                if remaining_length <= buf.len() {
                                    // Decode packet payload
                                    let payload_buf = buf.drain_to(remaining_length);
                                    let mut bytes = payload_buf.as_ref();
                                    let packet = match VariablePacket::decode_with(&mut bytes, Some(header)) {
                                        Ok(pkt) => {
                                            println!("[client.inner] Received packet: {:?}", pkt);
                                            let ignore = match pkt {
                                                VariablePacket::PingrespPacket(_) => ignore_pingresp,
                                                _ => false
                                            };
                                            if ignore { None} else { Some(pkt) }
                                        },
                                        // Print error message
                                        Err(_) => {
                                            // println!("[{}] Recv thread error", cloned_client_identifier);
                                            None
                                        }
                                    };
                                    fixed_header = None;
                                    if let Some(pkt) = packet  {
                                        inbox_sender.clone().send(pkt).wait().unwrap();
                                    }
                                } else {
                                    // debug!("Not enough bytes to decode Packet: length={}, fixed_header={:?}",
                                    //        self.buf.len(), self.fixed_header.unwrap());
                                    println!("[client] Not enough bytes to decode Packet: length={}, fixed_header={:?}",
                                             buf.len(), fixed_header);
                                }
                            }
                            None => {
                                // Just waiting for more data....
                                println!("[client] Just waiting for more data....")
                            }
                        };
                        Ok(())
                    }
                    ClientMsg::Shutdown(tx) => {
                        println!("ClientMsg::Shutdown");
                        tx.complete(());
                        Err(())
                    }
                }
            });
            let recv_packets = recv_packets.map_err(|e| {
                println!("future error(recv_packets): error={:?}, client={}",
                         e, &client_identifier);
                ()
            });

            let socket = TcpStream::connect(&addr, &core.handle());
            match tls_connector {
                Some(tls_connector) => {
                    let network = socket.and_then(|socket| {
                        tls_connector.connect_async("localhost", socket).map_err(native2io)
                    }).and_then(move |socket| {
                        handle_socket(socket, recv_tx, rx)
                    });
                    let network = network.map_err(|e| {
                        println!("future error(network): error={:?}, client={}",
                                 e, &client_identifier);
                        ()
                    });
                    let done = recv_packets.map(|_|()).join(network.map(|_|()));
                    let done = done.shared();
                    for n in 0..retry {
                        match core.run(done.clone()) {
                            Ok(_) => {
                                println!("Core run OK!!!");
                                break;
                            },
                            Err(e) => {
                                println!("Run core ERROR: error={:?}, retry={}, client={}",
                                         e, n, &client_identifier);
                                thread::sleep(time::Duration::from_millis(20));
                            }
                        }
                    }
                },
                None => {
                    let network = socket.and_then(move |socket| {
                        handle_socket(socket, recv_tx, rx)
                    });
                    let network = network.map_err(|e| {
                        println!("future error(network): error={:?}, client={}",
                                 e, &client_identifier);
                        ()
                    });
                    let done = recv_packets.map(|_|()).join(network.map(|_|()));
                    let done = done.shared();
                    for n in 0..retry {
                        match core.run(done.clone()) {
                            Ok(_) => {
                                println!("Core run OK!!!");
                                break;
                            },
                            Err(e) => {
                                println!("Run core ERROR: error={:?}, retry={}, client={}",
                                         e, n, &client_identifier);
                                thread::sleep(time::Duration::from_millis(20));
                            }
                        }
                    }
                }
            }
        });

        println!("Send connect packet");
        let mut connect_pkt = ConnectPacket::new("MQTT".to_owned(), self.client_identifier.clone());
        connect_pkt.set_clean_session(self.clean_session);
        connect_pkt.set_keep_alive(self.keep_alive as u16);
        if self.will.is_some() {
            connect_pkt.set_will(self.will.clone());
            connect_pkt.set_will_qos(self.will_qos.unwrap_or(0));
            connect_pkt.set_will_retain(self.will_retain.unwrap_or(false));
        }
        self.send(&VariablePacket::ConnectPacket(connect_pkt));

        self.inbox = Some(inbox);
        handler(self);
    }

    pub fn shutdown(&mut self) {
        if let Some(ref mut tx) = self.postman {
            tx.send(Vec::new()).wait().unwrap();
        }
        if let Some(ref mut tx) = self.recv_inbox {
            let (done_tx, done_rx) = oneshot::channel::<()>();
            tx.send(ClientMsg::Shutdown(done_tx)).wait().unwrap();
            done_rx.wait().unwrap();
        }
    }

    // pub fn subscribe(&mut self, subscriptions: Vec<(&str, u32)>) {}
    // pub fn unsubscribe(&mut self, subscriptions: Vec<&str>) {}
    // pub fn publish(&mut self, topic_name: &str, payload: Vec<u8>, qos: u32, retain: bool, dup: bool) {}

    pub fn subscribe(&mut self, subscriptions: Vec<(&str, u32)>) {
        let subscriptions = subscriptions.iter().map(|&(topic, qos)| {
            let qos = match qos {
                0 => QualityOfService::Level0,
                1 => QualityOfService::Level1,
                2 => QualityOfService::Level2,
                _ => panic!("Invalid qos")
            };
            (TopicFilter::new_checked(topic).unwrap(), qos)
        }).collect::<Vec<(TopicFilter, QualityOfService)>>();
        let packet = SubscribePacket::new(self.pkid(), subscriptions);
        self.send(&VariablePacket::SubscribePacket(packet));
    }

    pub fn unsubscribe(&mut self, subscriptions: Vec<&str>) {
        let subscriptions = subscriptions.iter().map(|topic| {
            TopicFilter::new_checked(*topic).unwrap()
        }).collect::<Vec<TopicFilter>>();
        let packet = UnsubscribePacket::new(self.pkid(), subscriptions);
        self.send(&VariablePacket::UnsubscribePacket(packet));
    }

    pub fn publish(&mut self, topic_name: &str, payload: Vec<u8>, qos: u32, retain: bool, dup: bool) {
        let topic_name = TopicName::new(topic_name.to_string()).unwrap();
        let pkid = self.pkid();
        let qos = match qos {
            0 => QoSWithPacketIdentifier::Level0,
            1 => QoSWithPacketIdentifier::Level1(pkid),
            2 => QoSWithPacketIdentifier::Level2(pkid),
            _ => panic!("Invalid qos")
        };
        let mut packet = PublishPacket::new(topic_name, qos, payload);
        packet.set_retain(retain);
        packet.set_dup(dup);
        self.send(&VariablePacket::PublishPacket(packet));
    }

    fn pkid(&mut self) -> u16 {
        if self.pkid == 65535 {
            self.pkid = 0;
        }
        self.pkid += 1;
        self.pkid
    }

    pub fn send(&mut self, packet: &VariablePacket) {
        if let Some(ref mut tx) = self.postman {
            let mut buf = Vec::new();
            packet.encode(&mut buf).unwrap();
            tx.send(buf).wait().unwrap();
            println!("Sent a packet: {:?}", packet);
        }
    }

    fn try_receive(&mut self) -> Option<VariablePacket> {
        match self.inbox {
            Some(ref mut inbox) => {
                let rv = future::poll_fn(|| inbox.poll());
                match rv.wait() {
                    Ok(x) => x,
                    Err(e) => {
                        println!("[client] inbox.poll Error: {:?}", e);
                        None
                    }
                }
            }
            None => None
        }
    }

    pub fn receive(&mut self) -> Option<VariablePacket> {
        loop {
            let packet = self.try_receive();
            if packet.is_some() {
                println!("[client] >>> Received a packet: {:?}", packet);
                return packet;
            } else {
                println!("[client] Waiting for next pakcket.");
                thread::sleep(time::Duration::from_millis(20));
            }
        }
    }
}

fn handle_socket<S>(socket: S,
                    recv_tx: mpsc::Sender<ClientMsg>,
                    rx: mpsc::Receiver<Vec<u8>>,
) -> BoxFuture<(), Error>
    where S: Io + Send + 'static {
    println!("Start socket");
    let (reader, writer) = socket.split();
    let reader = BufReader::new(reader);
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
        let recv_tx = recv_tx.clone();
        data.map(move |(reader, buf, n)| {
            let buf = buf.iter().take(n).cloned().collect();
            recv_tx.send(ClientMsg::Data(buf))
                .wait().unwrap();
            reader
        })
    });

    let socket_writer = rx.fold(writer, |writer, data| {
        println!("[client] Write data: {:?}", data);
        let amt = tokio_io::write_all(writer, data);
        let amt = amt.and_then(|(writer, data)| {
            if data.is_empty() {
                Err(Error::new(ErrorKind::BrokenPipe, "quit"))
            } else {
                Ok((writer, data))
            }
        });
        let amt = amt.map(|(writer, _)| writer);
        amt.map_err(|e| {
            println!("future error(socket_writer): {:?}", e);
            ()
        })
    });

    let socket_reader = socket_reader.map_err(|e| {
        println!("future error(socket_reader): {:?}", e);
        ()
    });
    socket_reader.map(|_| ()).select(socket_writer.map(|_| ())).then(|_| {
        // Connection closed!
        Ok(())
    }).boxed()
}
