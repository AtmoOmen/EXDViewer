//! Reads a shader package the way a partial store serves it, and checks the result against the file.

use std::sync::{Arc, Condvar, Mutex};
use std::task::{Context, Poll, Wake, Waker};

use ironworks::file::shpk::ShaderPackage;
use ironworks::{
    Ironworks,
    sqpack::{Install, SqPack},
};
use viewer::utils::fetch_range;

const API: &str = "https://xiviewer.app/api/global/2026.08.11.0000.0000/file";
const SQPACK: &str = "/home/asriel/.xlcore/ffxiv/game/sqpack";
const HEAD: u64 = 4096;

fn word(head: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(head[at..at + 4].try_into().unwrap())
}

fn main() {
    let ironworks = Ironworks::new().with_resource(SqPack::new(Install::at_sqpack(SQPACK)));
    block_on(async {
        for path in std::env::args().skip(1) {
            let whole = ironworks.file::<Vec<u8>>(&path).expect("the package");
            let url = format!("{API}/{path}");

            let head = fetch_range(&url, 0, Some(HEAD - 1)).await.expect("a head");
            assert_eq!(head.status, 206, "{path}");
            let (size, blobs, strings) = (
                word(&head.bytes, 12),
                word(&head.bytes, 16),
                word(&head.bytes, 20),
            );
            let mut bytes = head.bytes;
            if u64::from(blobs) > HEAD {
                bytes.extend(
                    fetch_range(&url, HEAD, Some(u64::from(blobs) - 1))
                        .await
                        .expect("the tables")
                        .bytes,
                );
            }
            bytes.truncate(blobs as usize);
            bytes.resize(strings as usize, 0);
            bytes.extend(
                fetch_range(&url, u64::from(strings), Some(u64::from(size) - 1))
                    .await
                    .expect("the strings")
                    .bytes,
            );
            assert_eq!(bytes.len(), size as usize, "{path}");
            assert_eq!(bytes[..blobs as usize], whole[..blobs as usize], "{path}");
            assert_eq!(bytes[strings as usize..], whole[strings as usize..], "{path}");

            let package = ShaderPackage::parse(&bytes).expect("a holed package");
            let base = package.blobs_offset() as u32;
            let mut filled = 0;
            for shader in package.shaders().iter().take(8) {
                let span = base + shader.blob_offset()..base + shader.blob_offset() + shader.blob_size();
                let blob = fetch_range(&url, u64::from(span.start), Some(u64::from(span.end) - 1))
                    .await
                    .expect("a blob")
                    .bytes;
                bytes[span.start as usize..span.end as usize].copy_from_slice(&blob);
                assert_eq!(blob, whole[span.start as usize..span.end as usize], "{path}");
                filled += blob.len();
            }
            println!(
                "{path}: {} of {size} bytes read as tables and strings, 8 blobs {filled} bytes, \
                 {} shaders",
                blobs as usize + (size - strings) as usize,
                package.shaders().len(),
            );
        }
    });
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
