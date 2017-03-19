

use std::str;
use std::str::FromStr;
use std::net::{SocketAddr};
use std::sync::mpsc;
use std::{thread, time};
// use std::fs::File;
// use std::io::Write;

// use futures::sync::mpsc::Receiver as FutureReceiver;
// use futures::{Async, Stream};
// use tempdir::TempDir;
// use openssl::ssl::{Ssl, SslContext, SslMethod};

use broker;
use mqtt::packet::{Packet, VariablePacket, QoSWithPacketIdentifier};
use mqtt::control::{ConnectReturnCode};
use native_tls::{TlsAcceptor, TlsConnector};

use client::{Client};

use common::tls::{contexts, contexts2};

fn _test_pubsub(url: &str, addr: &str,
                server_cx: Option<TlsAcceptor>, client_cs: Option<TlsConnector>) {
    let addr = SocketAddr::from_str(addr).unwrap();
    thread::spawn(move || broker::run_simple(addr, server_cx));
    thread::sleep(time::Duration::from_millis(20));

    let keep_alive = 2;
    let client_identifier = "(test_pubsub) client-1";
    let clean_session = true;
    let ignore_pingresp = true;
    let mut client = Client::new(url, client_cs,
                                 keep_alive,
                                 client_identifier, clean_session,
                                 ignore_pingresp,
                                 None, None, None);
    client.connect(5, |ref mut client| {
        let packet = client.receive().unwrap();
        println!("[inbox]: Packet={:?}", packet);
        if let VariablePacket::ConnackPacket(ref pkt) = packet {
            assert_eq!(pkt.connack_flags().session_present, false);
            assert_eq!(pkt.connect_return_code(), ConnectReturnCode::ConnectionAccepted);
        } else {
            panic!("First packet is not connack");
        }

        // * Simple pub/sub
        client.subscribe(vec![("a/b", 0)]);
        let _suback = client.receive().unwrap();

        client.publish("a/b", "this-is-ab".as_bytes().to_vec(), 0, false, false);
        let packet = client.receive().unwrap();
        println!("[inbox]: Packet={:?}", packet);
        if let VariablePacket::PublishPacket(ref pkt) = packet {
            assert_eq!(pkt.qos(), QoSWithPacketIdentifier::Level0);
            assert_eq!(pkt.topic_name(), "a/b");
            assert_eq!(str::from_utf8(pkt.payload()).unwrap(), "this-is-ab");
        } else {
            panic!("Not receive publish packet");
        }
        client.shutdown();
    });
}

#[test]
fn test_pubsub_without_tls() {
    let url = "ssl://127.0.0.1:1999";
    let addr = "127.0.0.1:1999";
    _test_pubsub(url, addr, None, None);
}


#[test]
fn test_pubsub_tls() {
    let url = "ssl://127.0.0.1:2000";
    let addr = "127.0.0.1:2000";
    let (server_cx, client_cs) = contexts();
    _test_pubsub(url, addr, Some(server_cx), Some(client_cs));
}

#[test]
fn test_retain() {
    let url = "ssl://127.0.0.1:2001";
    let addr = SocketAddr::from_str("127.0.0.1:2001").unwrap();
    let (server_cx, client_cs) = contexts();
    thread::spawn(move || broker::run_simple(addr, Some(server_cx)));
    thread::sleep(time::Duration::from_millis(20));

    let keep_alive = 2;
    let client_identifier = "(test_retain) client-1";
    let clean_session = true;
    let ignore_pingresp = true;
    let mut client = Client::new(url, Some(client_cs),
                                 keep_alive,
                                 client_identifier, clean_session,
                                 ignore_pingresp,
                                 None, None, None);
    client.connect(5, |ref mut client| {
        let _connack = client.receive().unwrap();
        let qos = 0;
        let retain = true;
        client.publish("a/b", "this-is-ab".as_bytes().to_vec(), qos, retain, false);

        for i in 0..3 {
            client.subscribe(vec![("a/b", qos)]);
            let _suback = client.receive().unwrap();

            let packet = client.receive().unwrap();
            println!("[inbox]: Packet({})={:?}", i, packet);
            if let VariablePacket::PublishPacket(ref pkt) = packet {
                assert_eq!(pkt.retain(), true);
                assert_eq!(str::from_utf8(pkt.payload()).unwrap(), "this-is-ab");
            } else {
                panic!("Not receive publish packet");
            }
            client.unsubscribe(vec!["a/b"]);
            let _unsuback = client.receive().unwrap();
        }

        // * Subscribe irrelevant topic
        client.subscribe(vec![("b/+", qos)]);
        let _suback = client.receive().unwrap();
        // assert_eq!(client.try_receive().is_some(), false);

        // * Subscribe wildcard topic.
        client.subscribe(vec![("a/+", qos)]);
        let _unsuback = client.receive().unwrap();

        let packet = client.receive().unwrap();
        println!("[inbox]: Packet={:?}", packet);
        if let VariablePacket::PublishPacket(ref pkt) = packet {
            assert_eq!(pkt.retain(), true);
            assert_eq!(pkt.topic_name(), "a/b");
            assert_eq!(str::from_utf8(pkt.payload()).unwrap(), "this-is-ab");
        } else {
            panic!("Not receive publish packet");
        }
        client.unsubscribe(vec!["a/+"]);
        let _unsuback = client.receive().unwrap();

        // * Clear retain message
        client.publish("a/b", Vec::new(), qos, retain, false);
        client.subscribe(vec![("a/b", qos)]);
        let _suback = client.receive().unwrap();

        // assert_eq!(client.try_receive().is_some(), false);
        client.shutdown();
    })
}

#[test]
fn test_last_will() {
    println!(">>> test_last_will() started");

    let url = "ssl://127.0.0.1:2002";
    let addr = SocketAddr::from_str("127.0.0.1:2002").unwrap();
    let (server_cx, client_cs, client_cs2) = contexts2();
    thread::spawn(move || broker::run_simple(addr, Some(server_cx)));
    thread::sleep(time::Duration::from_millis(20));

    let keep_alive = 2;
    let clean_session = true;
    let ignore_pingresp = true;

    let will_topic = "last/will/topic";
    let will_message = "This is a will message";

    let mut client_normal = Client::new(url, Some(client_cs),
                                        keep_alive,
                                        "(test_last_will) client-1", clean_session,
                                        ignore_pingresp,
                                        None, None, None);
    let cloned_will_topic = will_topic.clone();
    let cloned_will_message = will_message.clone();
    let (signal_tx, signal_rx) = mpsc::channel::<()>();
    let thread_normal = thread::spawn(move || {
        let client_identifier = "(test_last_will) client-1";
        client_normal.connect(5, move |ref mut client| {
            println!("[{}]: Receiving connack", client_identifier);
            let connack = client.receive().unwrap();
            println!("[{}.inbox]: Packet={:?}", client_identifier, connack);
            client.subscribe(vec![(cloned_will_topic, 0)]);
            let _suback = client.receive().unwrap();

            thread::sleep(time::Duration::from_millis(20));
            signal_tx.send(()).unwrap();

            let packet = client.receive().unwrap();
            println!("[{}.inbox]: Packet={:?}", client_identifier, packet);
            if let VariablePacket::PublishPacket(ref pkt) = packet {
                assert_eq!(str::from_utf8(pkt.payload()).unwrap(), cloned_will_message);
            } else {
                panic!("Not receve publish packet");
            }

            client.shutdown();
        });
        println!("[client-1]: thread finished");
    });

    let will = Some((will_topic, will_message.as_bytes().to_vec()));
    let mut client_will = Client::new(url, Some(client_cs2),
                                      keep_alive,
                                      "(test_last_will) client-2", clean_session,
                                      ignore_pingresp,
                                      will, None, None);
    client_will.connect(5, move |ref mut client| {
        let _connack = client.receive().unwrap();
        signal_rx.recv().unwrap();
        client.shutdown();
    });
    println!("Waiting for client-1 to join......");
    let _ = thread_normal.join();
}
