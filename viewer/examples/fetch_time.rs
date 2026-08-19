//! How long a run of reads through the web provider takes, and what it moves.
//!
//! `fetch_time <count>`

use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};
use std::time::Instant;

use viewer::utils::fetch;

const API: &str = "https://exd.camora.dev/api/global/2026.08.11.0000.0000/file";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";
const ZONE: &str = "bg/ex5/02_ykt_y6/twn/y6t1/";

fn main() {
    let count: usize = std::env::args()
        .nth(1)
        .and_then(|held| held.parse().ok())
        .unwrap_or(300);
    let list = std::fs::read_to_string(PATHS).expect("the path list");
    let paths: Vec<&str> = list
        .lines()
        .filter(|path| path.starts_with(ZONE) && path.ends_with(".mtrl"))
        .take(count)
        .collect();
    println!("{} files", paths.len());

    let at = Instant::now();
    let bytes: usize = block_on(futures_util::future::join_all(
        paths
            .iter()
            .map(|path| async move { fetch(format!("{API}/{path}")).await.map(|held| held.bytes.len()) }),
    ))
    .into_iter()
    .map(|held| held.expect("a file"))
    .sum();
    let took = at.elapsed();
    println!("{bytes} bytes in {took:.2?}");
}

struct Parked(Mutex<bool>, Condvar);

impl Wake for Parked {
    fn wake(self: Arc<Self>) {
        *self.0.lock().unwrap() = true;
        self.1.notify_one();
    }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let parked = Arc::new(Parked(Mutex::new(false), Condvar::new()));
    let waker = Waker::from(parked.clone());
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        if let Poll::Ready(held) = future.as_mut().poll(&mut context) {
            return held;
        }
        let mut woken = parked.0.lock().unwrap();
        while !*woken {
            woken = parked.1.wait(woken).unwrap();
        }
        *woken = false;
    }
}
