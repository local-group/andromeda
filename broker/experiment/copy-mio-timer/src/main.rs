extern crate mio;
extern crate chrono;

use std::mem;
use std::time::{Duration, Instant};
use std::str::FromStr;
use std::net::SocketAddr;

use mio::timer::{Timer};


#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub enum SessionTimerPacketType {
    // [QoS.1.send] Receive PUBACK timeout
    RecvPubackTimeout,
    // [QoS.2.send] Receive PUBREC timeout
    RecvPubrecTimeout,
    // [QoS.2.send] Receive PUBCOMP timeout
    RecvPubcompTimeout,
    // [QoS.2.recv] Receive PUBREL timeout
    RecvPubrelTimeout,
}

#[derive(Debug, Eq, PartialEq, Hash, Clone)]
pub enum SessionTimerPayload {
    // SocketAddr => Client addr
    // u16 => packet_identifier (pkid)
    RecvPacketTimer(SessionTimerPacketType, SocketAddr, u16),
    // Receive PINGREQ timeout
    KeepAliveTimer(SocketAddr),
    // Decode one packet timeout (maybe useless ??)
    // DecodePacketTimer(SocketAddr),
}

fn main() {
    let mut timer = Timer::default();

    println!("[size.SessionTimerPayload]={}", mem::size_of::<SessionTimerPayload>());
    println!("[size.Instant]={}", mem::size_of::<Instant>());
    let payload = SessionTimerPayload::RecvPacketTimer(
        SessionTimerPacketType::RecvPubackTimeout,
        SocketAddr::from_str("127.0.0.1:22").unwrap(),
        1234
    );

    let now = Instant::now();
    for i in 0..65537 {
        let result = timer.set_timeout(Duration::from_secs((15 + i) % 15), payload.clone());
        if result.is_err() {
            println!("[set_timeout.Error]: i={}, result={:?}", i, result);
        }
    }
    let elapsed = now.elapsed();
    println!("[set_timeout.cost] count=65536, elapsed={}s, {:?}ms",
             elapsed.as_secs(), elapsed.subsec_nanos() / 1000_000);

    // let mut other_timer = Builder::default().capacity(65536 * 2).build();
    // for i in 0..65536 {
    // }
}
