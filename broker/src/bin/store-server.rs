#[macro_use]
extern crate log;
extern crate broker;
extern crate env_logger;

use std::str::FromStr;
use std::net::{SocketAddr};

fn main() {
    env_logger::init().unwrap();
    let addr = "127.0.0.1:8884";
    broker::store::run(SocketAddr::from_str(addr).unwrap(), None);
}
