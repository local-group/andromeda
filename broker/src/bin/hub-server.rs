#[macro_use]
extern crate log;
extern crate broker;
extern crate env_logger;
extern crate native_tls;
extern crate time;
extern crate ansi_term;

use std::fs::File;
use std::io::{Read};
use std::env;
use std::str::FromStr;
use std::net::{SocketAddr};

use ansi_term::{Color};
use log::{LogRecord, LogLevel};
use env_logger::LogBuilder;
use native_tls::{TlsAcceptor, Pkcs12};

fn main() {
    let formatter = |record: &LogRecord| {
        let t = time::now();
        let location = record.location();
        let level = match record.level() {
            LogLevel::Trace => Color::Purple.paint("TRACE"),
            LogLevel::Debug => Color::Blue.paint("DEBUG"),
            LogLevel::Info => Color::Green.paint("INFO "),
            LogLevel::Warn => Color::Yellow.paint("WARN "),
            LogLevel::Error => Color::Red.paint("ERROR")
        };
        format!("{} - {} {}",
                Color::RGB(0x88, 0x88, 0x88).paint(
                    format!("{},{:03} - {}:{}",
                            time::strftime("%Y-%m-%d %H:%M:%S", &t).unwrap(),
                            t.tm_nsec / 1000_000,
                            location.module_path(),
                            location.line())),
                level,
                record.args()
        )
    };
    let mut builder = LogBuilder::new();
    builder.format(formatter);
    if let Ok(s) = ::std::env::var("RUST_LOG") {
        builder.parse(&s);
    }
    builder.init().unwrap();

    let addr = "127.0.0.1:8883";
    let store_addr = "127.0.0.1:8884";
    debug!("Listening on: {:?}", addr);
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
