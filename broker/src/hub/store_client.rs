use std::thread;
use std::sync::mpsc::{channel, Sender};

use futures::sync::mpsc;

use super::{LocalRouterMsg};
use common::{StoreRequest, StoreResponse, NetClient};

pub fn run(
    url: &str,
    store_client_rx: mpsc::Receiver<StoreRequest>,
    local_router_tx: Sender<LocalRouterMsg>
) {
    let (tx, rx) = channel::<StoreResponse>();
    let client = NetClient::new(url, None);
    let handle = thread::spawn(move|| {
        loop {
            match rx.recv().unwrap() {
                StoreResponse::Publish(user_id, packet) => {
                    let msg = LocalRouterMsg::Publish(user_id, packet);
                    local_router_tx.send(msg).unwrap();
                }
                StoreResponse::Retains(_, _, _, _) => {
                    // TODO:
                }
            }
        }
    });
    client.start_loop(store_client_rx, tx);
    handle.join().unwrap();
}
