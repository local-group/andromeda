extern crate tokio_timer;
extern crate futures;

use tokio_timer::*;
use futures::*;
use std::time::*;

fn use_tokio_timer() {
    // Create a new timer with default settings. While this is the easiest way
    // to get a timer, usually you will want to tune the config settings for
    // your usage patterns.
    let timer = Timer::default();

    // Set a timeout that expires in 500 milliseconds
    let sleep = timer.sleep(Duration::from_millis(500));

    // Use the `Future::wait` to block the current thread until `Sleep`
    // future completes.
    //
    let t1 = Instant::now();
    println!("Sleeping......");
    sleep.wait().unwrap();
    println!("Sleep done, cost={:?}", Instant::now() - t1);
}

pub fn main() {
    use_tokio_timer();
}
