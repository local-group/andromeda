
extern crate broker;
extern crate client;
extern crate mqtt;
#[macro_use]
extern crate log;
extern crate env_logger;
extern crate threadpool;
extern crate net2;
extern crate native_tls;
extern crate futures;
extern crate tokio_core;
extern crate tokio_tls;

extern crate rustc_serialize;
extern crate tempdir;

#[macro_use]
extern crate cfg_if;

mod common;
mod test_simple;
mod test_local_router;
mod test_global_retain;
