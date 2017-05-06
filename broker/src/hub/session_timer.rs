
use std::thread;
use std::time::{Duration, Instant};
use std::sync::mpsc::{Sender, Receiver};
use std::collections::{HashMap};

use mio::timer::{Builder, Timeout};

use super::{ClientSessionMsg, SessionTimerAction, SessionTimerPayload};

pub fn run(inbox: Receiver<(SessionTimerAction, SessionTimerPayload)>,
           client_session_tx: Sender<ClientSessionMsg>) {
    let tick_ms: u64 = 50;
    let mut capacity: usize = 4096;
    let num_slots = (100 / tick_ms * 256) as usize;
    let mut timer = Builder::default()
        .num_slots(num_slots)
        .tick_duration(Duration::from_millis(tick_ms))
        .capacity(capacity)
        .build::<SessionTimerPayload>();
    let mut timeouts = HashMap::<SessionTimerPayload, (Instant, Timeout)>::new();

    let inbox_count = 60;
    let timer_count = 40;
    let mut last_resize = Instant::now();
    // 15 minutes
    let max_shrink_interval: u64 = 15 * 60;

    loop {
        let mut inbox_busy = true;
        let mut timer_busy = true;

        // Receive timer messages from `client_session`
        for _ in 0..inbox_count {
            match inbox.try_recv() {
                Ok((action, payload)) => {
                    debug!("Set timeout: action={:?}, payload={:?}", action, payload);
                    match action {
                        SessionTimerAction::Set(end) => {
                            if let Some((_, timeout)) = timeouts.remove(&payload) {
                                // timer.cancel_timeout() is necessary!
                                timer.cancel_timeout(&timeout);
                            }
                            let now = Instant::now();
                            if end > now {
                                if let Ok(timeout) = timer.set_timeout(end - now, payload.clone()) {
                                    timeouts.insert(payload, (end, timeout));
                                }
                            } else {
                                // Already timeout (resize cost much time)
                                client_session_tx.send(ClientSessionMsg::Timeout(payload)).unwrap();
                            }
                        }
                        SessionTimerAction::Cancel => {
                            if let Some((_, timeout)) = timeouts.remove(&payload) {
                                timer.cancel_timeout(&timeout);
                            }
                        }
                    }
                }
                Err(_) => {
                    inbox_busy = false;
                    break;
                }
            }
        }

        // Poll timeouts and send them to client_session
        for _ in 0..timer_count {
            match timer.poll() {
                Some(payload) => {
                    debug!("Timeout: payload={:?}", payload);
                    client_session_tx.send(ClientSessionMsg::Timeout(payload)).unwrap();
                }
                None => {
                    timer_busy = false;
                    break;
                }
            }
        }

        // Resize timer and timeouts
        let should_resize = if timeouts.len() >= capacity + inbox_count {
            capacity *= 2;
            true
        } else if capacity > 4096 && timeouts.len() < capacity / 4 &&
            (Instant::now() - last_resize).as_secs() > max_shrink_interval {
            capacity /= 2;
            true
        } else { false };

        if should_resize {
            let mut new_timer = Builder::default()
                .num_slots(num_slots)
                .tick_duration(Duration::from_millis(tick_ms))
                .capacity(capacity)
                .build();
            let mut new_timeouts = HashMap::new();
            {
                let mut now = Instant::now();
                for (i, (payload, (end, _))) in timeouts.drain().enumerate() {
                    if (i+1) % 512 == 0 {
                        now = Instant::now();
                    }
                    if end > now {
                        if let Ok(timeout) = new_timer.set_timeout(end - now, payload.clone()) {
                            new_timeouts.insert(payload, (end, timeout));
                        }
                    } else {
                        // Already timeout (resize cost much time)
                        client_session_tx.send(ClientSessionMsg::Timeout(payload)).unwrap();
                    }
                }
            }
            timer = new_timer;
            timeouts = new_timeouts;
            last_resize = Instant::now();
        } else if !(inbox_busy || timer_busy) {
            thread::sleep(Duration::from_millis(100));
        }
    }
}
