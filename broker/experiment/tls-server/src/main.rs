extern crate native_tls;

use native_tls::{Pkcs12, TlsAcceptor, TlsStream};
use std::fs::File;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::thread;
use std::env;
use std::str;


/// [Client]: openssl s_client -CAfile ./certificate/CA/root-ca.cert -connect 127.0.0.1:8443
fn main() {
    let mut file = File::open(env::args().nth(1).unwrap()).unwrap();
    let mut pkcs12 = vec![];
    file.read_to_end(&mut pkcs12).unwrap();
    let pkcs12 = Pkcs12::from_der(&pkcs12, "public").unwrap();

    let acceptor = TlsAcceptor::builder(pkcs12).unwrap().build().unwrap();
    let acceptor = Arc::new(acceptor);

    let listener = TcpListener::bind("0.0.0.0:8443").unwrap();

    fn handle_client(stream: &mut TlsStream<TcpStream>) {
        // ...
        let mut buf = Vec::new();
        stream.write_all(b"[body=abcedf]").unwrap();
        stream.read_to_end(&mut buf).unwrap();
        println!("[handle_client]: stream={:?}, received={}",
                 stream, str::from_utf8(&buf).unwrap());
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let acceptor = acceptor.clone();
                thread::spawn(move || {
                    let mut stream = acceptor.accept(stream).unwrap();
                    handle_client(&mut stream);
                });
            }
            Err(e) => {
                /* connection failed */
                println!("[ERROR]: {:?}", e);
            }
        }
    }
}
