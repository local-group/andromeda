
use std::{thread, time};
use std::sync::mpsc::{Sender};

use futures::sync::mpsc;

pub fn run_epoll(
    _router_follower_rx: mpsc::Receiver<super::RouterFollowerMsg>,
    _local_router_tx: Sender<super::LocalRouterMsg>,
) {
    thread::sleep(time::Duration::from_secs(180));
}
