//! What reading and decoding a zone's files costs the thread driving them, inline and pooled.
//!
//! `decode_cost bg/ex1/01_roc_r2/dun/r2d1/ [in flight]`

use std::sync::Arc;
use std::sync::{Condvar, Mutex};
use std::task::{Context, Wake, Waker};
use std::time::{Duration, Instant};

use futures_util::stream::{FuturesUnordered, StreamExt};
use ironworks::file::File as _;
use ironworks::file::mdl::ModelContainer;
use ironworks::sqpack::{Install, SqPack};
use viewer::utils::tex_loader::decode_preview_sized;

const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const PATHS: &str = "/home/asriel/Code/ironworks-formats/paths.txt";
const TEXTURE_SIZE: u16 = 256;

fn one(pack: &SqPack<Install>, path: &str) -> usize {
    let Ok(mut file) = pack.file(path) else {
        return 0;
    };
    let mut bytes = Vec::new();
    if std::io::Read::read_to_end(&mut file, &mut bytes).is_err() {
        return 0;
    }
    let size = bytes.len();
    if path.ends_with(".tex") {
        let _ = decode_preview_sized(&bytes, path, Some(TEXTURE_SIZE));
    } else if let Ok(container) = ModelContainer::read(std::io::Cursor::new(bytes)) {
        for mesh in container.model(ironworks::file::mdl::Lod::High).meshes() {
            let _ = mesh.attributes();
            let _ = mesh.indices();
        }
    }
    size
}

fn main() {
    let zone = std::env::args().nth(1).expect("a zone prefix");
    let flight: usize = std::env::args()
        .nth(2)
        .and_then(|held| held.parse().ok())
        .unwrap_or(24);
    let list = std::fs::read_to_string(PATHS).expect("the path list");
    let wanted: Vec<String> = list
        .lines()
        .filter(|path| path.starts_with(&zone))
        .filter(|path| path.ends_with(".tex") || path.ends_with(".mdl"))
        .map(str::to_owned)
        .collect();
    println!("{} files under {zone}", wanted.len());

    let pack = Arc::new(SqPack::new(Install::at_sqpack(SQPACK)));
    // Warms the index caches, so a run measures reads rather than the first lookup in a package.
    for path in wanted.iter().take(4) {
        one(&pack, path);
    }

    let mut bytes = 0;
    let inline = drive(async {
        for path in &wanted {
            bytes += one(&pack, path);
        }
    });
    println!(
        "inline: {:.2?} wall, {:.2?} driving, {:.2?} longest, {:.1} MiB",
        inline.0,
        inline.1,
        inline.2,
        bytes as f64 / 1048576.0
    );

    let pooled = drive(async {
        let mut held = FuturesUnordered::new();
        let mut next = wanted.iter();
        loop {
            while held.len() < flight {
                let Some(path) = next.next() else { break };
                let (pack, path) = (pack.clone(), path.clone());
                held.push(blocking::unblock(move || one(&pack, &path)));
            }
            if held.next().await.is_none() {
                break;
            }
        }
    });
    println!(
        "pooled ({flight} in flight): {:.2?} wall, {:.2?} driving, {:.2?} longest",
        pooled.0, pooled.1, pooled.2
    );

    for pooled in [false, true] {
        for drain in [false, true] {
            let (wall, painted) = paint(&pack, &wanted, flight, drain, pooled);
            println!(
                "{} read, {}: {:.2?} over {painted} frames",
                if pooled { "pooled" } else { "inline" },
                if drain { "drained a frame" } else { "one tick a frame" },
                wall
            );
        }
    }
}

/// The app's own loop: a frame refills what is in flight, drives the local executor, and paints.
/// `poll_promise::tick_local` polls one task, so without draining a frame carries one file.
fn paint(
    pack: &Arc<SqPack<Install>>,
    wanted: &[String],
    flight: usize,
    drain: bool,
    pooled: bool,
) -> (Duration, usize) {
    let mut live: Vec<poll_promise::Promise<usize>> = Vec::new();
    let mut next = wanted.iter();
    let mut painted = 0;
    let mut spent = false;
    let at = Instant::now();
    loop {
        while live.len() < flight {
            let Some(path) = next.next() else {
                spent = true;
                break;
            };
            let (pack, path) = (pack.clone(), path.clone());
            live.push(poll_promise::Promise::spawn_local(async move {
                match pooled {
                    true => blocking::unblock(move || one(&pack, &path)).await,
                    false => one(&pack, &path),
                }
            }));
        }
        if drain {
            let until = Instant::now() + Duration::from_millis(4);
            while poll_promise::tick_local() && Instant::now() < until {}
        } else {
            poll_promise::tick_local();
        }
        live.retain(|held| held.ready().is_none());
        painted += 1;
        if spent && live.is_empty() {
            return (at.elapsed(), painted);
        }
        std::thread::sleep(Duration::from_micros(16_667));
    }
}

/// Runs a future to completion, reporting the wall clock it took, how much of that the driving
/// thread itself spent inside it, and the longest single stretch it was held for. In the app that
/// thread is the one that draws, so the last two are frames that did not paint.
fn drive<F: Future>(future: F) -> (Duration, Duration, Duration) {
    let parked = Arc::new(Parked(Mutex::new(false), Condvar::new()));
    let waker = Waker::from(parked.clone());
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    let (mut driving, mut longest) = (Duration::ZERO, Duration::ZERO);
    let at = Instant::now();
    loop {
        let poll = Instant::now();
        let ready = future.as_mut().poll(&mut context).is_ready();
        let took = poll.elapsed();
        driving += took;
        longest = longest.max(took);
        if ready {
            return (at.elapsed(), driving, longest);
        }
        let mut woken = parked.0.lock().unwrap();
        while !*woken {
            woken = parked.1.wait(woken).unwrap();
        }
        *woken = false;
    }
}

struct Parked(Mutex<bool>, Condvar);

impl Wake for Parked {
    fn wake(self: Arc<Self>) {
        *self.0.lock().unwrap() = true;
        self.1.notify_one();
    }
}
