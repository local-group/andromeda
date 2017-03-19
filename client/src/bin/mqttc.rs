extern crate mqtt;
#[macro_use]
extern crate log;
extern crate env_logger;
extern crate clap;
extern crate uuid;
extern crate time;
extern crate ansi_term;
extern crate rustyline;

use std::net::TcpStream;
use std::io::{self, Write};
use std::collections::{HashMap, LinkedList};
use std::sync::mpsc::{channel, Sender};
use std::thread;
use std::process;
use std::time::{Duration};
use std::str;
use std::fmt;

use clap::{App, Arg};
use ansi_term::Colour::{Green, Blue, Red};
use rustyline::error::ReadlineError;
use rustyline::Editor;
use rustyline::completion::{Completer};

use uuid::Uuid;

use mqtt::{Encodable, Decodable, QualityOfService};
use mqtt::packet::*;
use mqtt::control::variable_header::ConnectReturnCode;
use mqtt::{TopicFilter, TopicName};


const CMD_STATUS      : &'static str = "status";
const CMD_GET         : &'static str = "get";
const CMD_SUBSCRIBE   : &'static str = "subscribe";
const CMD_UNSUBSCRIBE : &'static str = "unsubscribe";
const CMD_PUBLISH     : &'static str = "publish";
const CMD_HELP        : &'static str = "help";
const CMD_EXIT        : &'static str = "exit";


#[derive(Clone)]
struct MyCompleter<'a> {
    commands: &'a Vec<&'static str>
}

impl<'a> MyCompleter<'a> {
    pub fn new(commands: &'a Vec<&'static str>) -> MyCompleter<'a> {
        MyCompleter{commands:commands}
    }
}

impl<'a> Completer for MyCompleter<'a> {
    fn complete(&self, line: &str, _: usize) -> rustyline::Result<(usize, Vec<String>)> {
        let mut results: Vec<String> = Vec::new();
        for c in self.commands {
            if c.starts_with(line) {
                results.push(c.to_string());
            }
        }
        Ok((0, results))
    }
}


enum UnsubscribeTopic {
    Normal(String),
    All
}

struct LocalStatus{
    last_pingresp: Option<time::Tm>,
    subscribes: Vec<(TopicFilter, QualityOfService)>,
    counts: HashMap<String, usize>
}

impl fmt::Debug for LocalStatus {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        let pingresp_str = match self.last_pingresp {
            Some(v) => format!("{}", v.strftime("%Y-%m-%d %H:%M:%S").unwrap()),
            None => "None".to_string()
        };
        write!(fmt, "LastPingresp=[{}], Subscribes={:?}, Counts={:?}",
               pingresp_str , self.subscribes, self.counts)
    }
}

type LocalCount = usize;
type LocalMessages = Option<Vec<PublishPacket>>;

enum Action {
    Subscribe(TopicFilter, QualityOfService),
    Unsubscribe(UnsubscribeTopic),
    Publish(TopicName, String, bool),
    Receive(VariablePacket),
    Status(Sender<LocalStatus>),
    // TopicMessageCount(TopicName, Sender<usize>),
    Get(TopicName, usize, Sender<LocalMessages>)
}


fn generate_client_id() -> String {
    format!("/MQTT/rust/{}", Uuid::new_v4().to_simple_string())
}

fn print_error(s: &str) {
    println!("{}: {}", Red.paint("[Error]"), s);
}

fn main() {
    env_logger::init().unwrap();

    let matches = App::new("client")
                      .author("Y. T. Chung <zonyitoo@gmail.com>")
                      .arg(Arg::with_name("SERVER")
                               .short("S")
                               .long("server")
                               .default_value("127.0.0.1:1993")
                               .takes_value(true)
                               .help("MQTT server address (host:port)"))
                      // .arg(Arg::with_name("SUBSCRIBE")
                      //          .short("s")
                      //          .long("subscribe")
                      //          .takes_value(true)
                      //          .multiple(true)
                      //          .required(true)
                      //          .help("Channel filter to subscribe"))
                      .arg(Arg::with_name("USER_NAME")
                               .short("u")
                               .long("username")
                               .takes_value(true)
                               .help("Login user name"))
                      .arg(Arg::with_name("PASSWORD")
                               .short("p")
                               .long("password")
                               .takes_value(true)
                               .help("Password"))
                      .arg(Arg::with_name("CLIENT_ID")
                               .short("i")
                               .long("client-identifier")
                               .takes_value(true)
                               .help("Client identifier"))
                      .get_matches();

    let server_addr = matches.value_of("SERVER").unwrap();
    let client_id = matches.value_of("CLIENT_ID")
                           .map(|x| x.to_owned())
                           .unwrap_or_else(generate_client_id);

    print!("Connecting to {:?} ... ", server_addr);
    io::stdout().flush().unwrap();
    let mut stream = TcpStream::connect(server_addr).unwrap();
    println!("Connected!");

    let keep_alive = 10;
    println!("Client identifier {:?}", client_id);
    let mut conn = ConnectPacket::new("MQTT".to_owned(), client_id.to_owned());
    conn.set_clean_session(true);
    conn.set_keep_alive(keep_alive);
    let mut buf = Vec::new();
    info!("[Send]: {:?}", conn);
    conn.encode(&mut buf).unwrap();
    stream.write_all(&buf[..]).unwrap();

    let connack = ConnackPacket::decode(&mut stream).unwrap();
    info!("[Recv]: {:?}", connack);
    trace!("CONNACK {:?}", connack);

    if connack.connect_return_code() != ConnectReturnCode::ConnectionAccepted {
        panic!("Failed to connect to server, return code {:?}",
               connack.connect_return_code());
    }

    let (tx, rx) = channel::<Action>();
    let cloned_tx = tx.clone();

    // MQTT Background
    let mut cloned_stream = stream.try_clone().unwrap();
    thread::spawn(move || {
        let mut last_ping_time = 0;
        let mut next_ping_time = last_ping_time + (keep_alive as f32 * 0.9) as i64;
        loop {
            let current_timestamp = time::get_time().sec;
            if keep_alive > 0 && current_timestamp >= next_ping_time {
                // println!("Sending PINGREQ to broker");

                let pingreq_packet = PingreqPacket::new();

                let mut buf = Vec::new();
                pingreq_packet.encode(&mut buf).unwrap();
                debug!("[Send]: {:?}", pingreq_packet);
                cloned_stream.write_all(&buf[..]).unwrap();

                last_ping_time = current_timestamp;
                next_ping_time = last_ping_time + (keep_alive as f32 * 0.9) as i64;
                thread::sleep(Duration::new((keep_alive / 2) as u64, 0));
            }
        }
    });

    // Receive packets
    let mut cloned_stream = stream.try_clone().unwrap();
    thread::spawn(move || {
        // println!("Receiving messages!!!!!");
        loop {
            let packet = match VariablePacket::decode(&mut cloned_stream) {
                Ok(pk) => pk,
                Err(err) => {
                    error!("Error in receiving packet {:?}", err);
                    continue;
                }
            };
            debug!("[Recv]: {:?}", packet);
            let _ = cloned_tx.send(Action::Receive(packet.clone()));
        }
    });

    // Process actions
    let mut cloned_stream = stream.try_clone().unwrap();
    thread::spawn(move || {
        // println!("Processing messages!!!!!");
        let mut pkid: u16 = 0;
        let mut last_pingresp:Option<time::Tm> = None;
        let mut subscribes: Vec<(TopicFilter, QualityOfService)> = Vec::new();
        let mut mailbox: HashMap<TopicName, LinkedList<PublishPacket>> = HashMap::new();

        let mut get_pkid = || {
            pkid += 1;
            pkid
        };

        // Action process loop
        loop {
            let action = rx.recv().unwrap();
            match action {
                Action::Publish(topic_name, msg, retain) => {
                    let payload = if msg == "EMPTY" { Vec::new() } else { msg.as_bytes().to_vec() };
                    let mut publish_packet = PublishPacket::new(
                        topic_name.clone(), QoSWithPacketIdentifier::Level0, payload);
                    publish_packet.set_retain(retain);
                    let mut buf = Vec::new();
                    publish_packet.encode(&mut buf).unwrap();
                    debug!("[Send]: {:?}", publish_packet);
                    cloned_stream.write_all(&buf[..]).unwrap();
                }
                Action::Subscribe(topic_filter, qos) => {
                    let sub = SubscribePacket::new(get_pkid(), vec![(topic_filter.clone(), qos)]);
                    let mut buf = Vec::new();
                    sub.encode(&mut buf).unwrap();
                    debug!("[Send]: {:?}", sub);
                    cloned_stream.write_all(&buf[..]).unwrap();
                    subscribes.push((topic_filter, qos));
                }
                Action::Unsubscribe(unsub_topic) => {
                    let topics = match unsub_topic {
                        UnsubscribeTopic::Normal(topic) => {
                            vec![topic]
                        }
                        UnsubscribeTopic::All => {
                            let topics: Vec<String> = Vec::new();
                            topics
                        }
                    };
                    let topics = topics.iter().map(|topic| {
                        TopicFilter::new(topic.to_string())
                    }).collect();
                    let unsub = UnsubscribePacket::new(get_pkid(), topics);
                    let mut buf = Vec::new();
                    unsub.encode(&mut buf).unwrap();
                    cloned_stream.write_all(&buf[..]).unwrap();
                }
                Action::Receive(packet) => {
                    match &packet {
                        &VariablePacket::PublishPacket(ref packet) => {
                            let topic_name = TopicName::new(packet.topic_name().to_string()).unwrap();
                            mailbox
                                .entry(topic_name.clone())
                                .or_insert(LinkedList::new())
                                .push_back(packet.clone());
                        }
                        &VariablePacket::PingreqPacket(..) => {
                            debug!("Ping request recieved");
                            let pingresp = PingrespPacket::new();
                            debug!("Sending Ping response {:?}", pingresp);
                            debug!("[Send]: {:?}", pingresp);
                            pingresp.encode(&mut cloned_stream).unwrap();
                        }
                        &VariablePacket::PingrespPacket(..) => {
                            // println!("Receiving PINGRESP from broker ..");
                            last_pingresp = Some(time::now());
                        }
                        &VariablePacket::SubscribePacket(ref packet) => {
                            debug!("Subscribe packet received: {:?}", packet.payload().subscribes());
                        }
                        &VariablePacket::UnsubscribePacket(ref packet) => {
                            debug!("Unsubscribe packet received: {:?}", packet.payload().subscribes());
                        }
                        &VariablePacket::DisconnectPacket(..) => {
                            info!("Disconnecting...");
                            break;
                        }
                        _ => {
                            // Ignore other packets in pub client
                        }
                    }
                }
                Action::Status(sender) => {
                    let mut counts = HashMap::new();
                    for (topic, ref msgs) in &mailbox {
                        counts.insert(topic.to_string(), msgs.len());
                    }
                    let _ = sender.send(LocalStatus{
                        last_pingresp: last_pingresp,
                        subscribes: subscribes.clone(),
                        counts: counts
                    });
                }
                // Action::TopicMessageCount(topic_name, sender) => {
                //     if let Some(ref messages) = mailbox.get(&topic_name) {
                //         let _ = sender.send(messages.len());
                //     } else {
                //         let _ = sender.send(0);
                //     }
                // }
                Action::Get(topic_name, count, sender) => {
                    if let Some(ref mut messages) = mailbox.get_mut(&topic_name) {
                        let mut msgs = Vec::new();
                        let count = if messages.len() > count { count } else { messages.len() };
                        for _ in 0..count {
                            if let Some(msg) = messages.pop_front() {
                                msgs.push(msg);
                            } else {
                                break;
                            }
                        }
                        let _ = sender.send(Some(msgs));
                    } else {
                        let _ = sender.send(None);
                    }
                }
            }
        }

    });

    let (status_sender, status_receiver) = channel::<LocalStatus>();
    // let (count_sender, count_receiver) = channel::<LocalCount>();
    let (message_sender, message_receiver) = channel::<LocalMessages>();

    let commands = vec![
        CMD_STATUS,
        CMD_GET,
        CMD_SUBSCRIBE,
        CMD_UNSUBSCRIBE,
        CMD_PUBLISH,
        CMD_HELP,
        CMD_EXIT
    ];
    let completer = MyCompleter::new(&commands);
    let mut rl = Editor::new();
    rl.set_completer(Some(&completer));
    for c in &commands {
        rl.add_history_entry(&c);
    }

    loop {
        io::stdout().flush().unwrap();

        let readline = match rl.readline(format!("{} ", Blue.paint("mqttc>")).as_ref()) {
            Ok(line) => line,
            Err(ReadlineError::Interrupted) => {
                println!("{}", Green.paint("CTRL-C"));
                process::exit(0);
            },
            Err(ReadlineError::Eof) => {
                println!("{}", Green.paint("CTRL-D"));
                process::exit(0);
            },
            Err(err) => {
                print_error(&format!("{:?}", err));
                process::exit(-1);
            }
        };

        let command = readline.trim();
        let parts: Vec<&str> = command.splitn(2, ' ').collect();
        if let Some(action) = parts.first() {
            match *action {
                CMD_STATUS => {
                    let _ = tx.send(Action::Status(status_sender.clone()));
                    let status = status_receiver.recv().unwrap();
                    println!("{}: {:?}", Green.paint("[Status]"), status);
                    rl.add_history_entry(&command);
                }
                CMD_GET => {
                    if parts.len() > 1 {
                        let arg_parts: Vec<&str> = parts[1].split(' ').collect();
                        let topic_str = arg_parts[0];

                        let topic_name = TopicName::new(topic_str.to_string()).unwrap();
                        let count: usize = if arg_parts.len() > 1 {
                            let count_str = arg_parts[1];
                            count_str.parse().unwrap_or(1)
                        } else { 1 };
                        println!("{}: topic={}, count={}", Green.paint("[Get]"), &topic_name[..], count);
                        let _ = tx.send(Action::Get(topic_name, count, message_sender.clone()));
                        let rv = message_receiver.recv().unwrap();
                        if let Some(messages) = rv {
                            let mut message_strs: Vec<String> = Vec::new();
                            for msg in messages {
                                message_strs.push(String::from_utf8(msg.payload().to_owned()).unwrap())
                            }
                            println!("{}: {:?}", Green.paint("[Messages]"), message_strs);
                        } else {
                            println!("{}: None", Green.paint("[Messages]"));
                        }
                        rl.add_history_entry(&command);
                    } else {
                        print_error("topic missing!");
                    }
                }
                CMD_SUBSCRIBE => {
                    if parts.len() > 1 {
                        let args = parts[1].trim();
                        match TopicFilter::new_checked(args.to_string()) {
                            Ok(topic_filter) => {
                                let _ = tx.send(Action::Subscribe(topic_filter, QualityOfService::Level0));
                                rl.add_history_entry(&command);
                            }
                            Err(err) => {
                                print_error(&format!("{:?}", err))
                            }
                        }
                    } else {
                        print_error("Topic filter required!");
                    }
                }
                CMD_UNSUBSCRIBE => {
                    if parts.len() > 1 {
                        let args = parts[1].trim();
                        if args == "all" {
                            let _ = tx.send(Action::Unsubscribe(UnsubscribeTopic::All));
                        } else {
                            let _ = tx.send(Action::Unsubscribe(UnsubscribeTopic::Normal(args.to_string())));
                            // match TopicFilter::new_checked(args.to_string()) {
                            //     Ok(topic_filter) => {}
                            //     Err(err) => {
                            //         print_error(&format!("{:?}", err))
                            //     }
                            // }
                        }
                        rl.add_history_entry(&command);
                    } else {
                        print_error("Topic filter required!");
                    }
                }
                CMD_PUBLISH => {
                    if parts.len() > 1 {
                        let args = parts[1].trim();
                        let arg_parts: Vec<&str> = args.split(' ').collect();
                        if arg_parts.len() > 1 {
                            println!("{}: topic={}, message={}", Green.paint("[Publishing]"),
                                     arg_parts[0], arg_parts[1]);

                            let topic_name = TopicName::new(arg_parts[0].to_string()).unwrap();
                            let retain = arg_parts.len() >= 3 && vec!["1", "true", "yes"].contains(&arg_parts[2]);
                            let _ = tx.send(Action::Publish(topic_name, arg_parts[1].to_string(), retain));
                            rl.add_history_entry(&command);
                        } else {
                            print_error("message missing!");
                        }
                    } else {
                        print_error("not enough arguments!");
                    }
                }
                CMD_HELP => {
                    println!("* {status}                     Query mqtt status information \n\
                              * {get} TOPIC [COUNT]          Get some messages from a topic \n\
                              * {subscribe} TOPIC [QOS]      Subscribe a topic \n\
                              * {unsubscribe} TOPIC          Unsubscribe a topic (can be `all`) \n\
                              * {publish} TOPIC MESSAGE      Publish message to a topic \n\
                              * {help}                       Print help \n\
                              * {exit}                       Exit this program",
                             status=Green.bold().paint(commands[0]),
                             get=Green.bold().paint(commands[1]),
                             subscribe=Green.bold().paint(commands[2]),
                             unsubscribe=Green.bold().paint(commands[3]),
                             publish=Green.bold().paint(commands[4]),
                             help=Green.bold().paint(commands[5]),
                             exit=Green.bold().paint(commands[6])
                    );
                }
                CMD_EXIT => {
                    println!("{}", Green.paint("[Bye!]"));
                    process::exit(0);
                }
                _ => {
                    print_error("Unknown command!");
                }
            }
        }
    }
}
