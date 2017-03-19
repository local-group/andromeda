#[macro_use]
extern crate log;
extern crate broker;
extern crate env_logger;
extern crate native_tls;

use std::fs::File;
use std::io::{Read};
use std::env;
use std::str::FromStr;
use std::net::{SocketAddr};

use native_tls::{TlsAcceptor, Pkcs12};

fn main() {
    env_logger::init().unwrap();
    let addr = "127.0.0.1:8883";
    let tls_acceptor = env::args().nth(1).map(|file_path| {
        debug!("pkcs12 file path: {}", file_path);
        let mut file = File::open(file_path).unwrap();
        let mut pkcs12 = vec![];
        file.read_to_end(&mut pkcs12).unwrap();
        let pkcs12 = Pkcs12::from_der(&pkcs12, "public").unwrap();
        TlsAcceptor::builder(pkcs12).unwrap().build().unwrap()
    });
    broker::run_simple(SocketAddr::from_str(addr).unwrap(), tls_acceptor);
}
