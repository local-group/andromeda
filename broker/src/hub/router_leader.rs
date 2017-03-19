
use std::{thread, time};

use futures::sync::mpsc;

pub fn run_epoll(
    _router_leader_rx: mpsc::Receiver<super::RouterLeaderMsg>,
) {
    thread::sleep(time::Duration::from_secs(180));
}
