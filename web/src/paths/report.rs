//! Paths a client found the game shipping under a name nobody has logged, on their way to
//! ResLogger.
//!
//! The list is already held here, so a name it carries costs nothing downstream: it is dropped
//! before anything leaves the box.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use anyhow::Result;
use mini_moka::sync::{Cache, CacheBuilder};
use serde::{Deserialize, Serialize};

use crate::{config::Report as ReportConfig, paths::PathIndex};

/// The first segment of every path the list carries.
const CATEGORIES: [&str; 12] = [
    "bg",
    "bgcommon",
    "chara",
    "common",
    "cut",
    "exd",
    "game_script",
    "music",
    "shader",
    "sound",
    "ui",
    "vfx",
];

/// Longest path accepted.
const MAX_LENGTH: usize = 256;

/// Paths per forwarded request. ResLogger's plugin sends this many and its server caps at 2000.
const BATCH: usize = 250;

/// Names remembered as already passed on, so several clients finding the same file cost one
/// forward rather than one each. Bounded, because the box is small and the list itself is 70 MB.
const REMEMBERED: u64 = 8192;

/// Clients tracked for rate limiting at once.
const CLIENTS: u64 = 4096;

const WINDOW: Duration = Duration::from_secs(60 * 60);

#[derive(Debug, Deserialize)]
pub struct Submission {
    pub paths: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct Outcome {
    /// Names neither the list nor a recent submission carried.
    new: usize,
    /// Names already known here, so nothing was sent for them.
    known: usize,
    /// Names that cannot be a game path.
    rejected: usize,
    /// Whether the new names were passed on, which the deployment can turn off.
    forwarded: bool,
}

/// A submitted path in the form the list is keyed on, or nothing when it cannot be a game path.
///
/// An unnamed file is drawn as its hash, so a synthesised name is eight hex digits and carries no
/// extension. Requiring one refuses both that and a directory only nameable by hash.
fn canonical(path: &str) -> Option<String> {
    let path = path.trim().trim_matches('/').to_lowercase();
    if path.len() > MAX_LENGTH
        || !path
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./-".contains(c))
    {
        return None;
    }

    let mut segments = path.split('/');
    if !CATEGORIES.contains(&segments.next()?) {
        return None;
    }
    let (stem, extension) = segments.next_back()?.rsplit_once('.')?;
    if stem.is_empty() || extension.is_empty() {
        return None;
    }
    segments
        .all(|segment| !segment.is_empty() && segment != "." && segment != "..")
        .then_some(path)
}

pub struct Collector {
    paths: Arc<PathIndex>,
    config: ReportConfig,
    client: reqwest::Client,
    forwarded: Cache<String, ()>,
    clients: Cache<String, (Instant, u32)>,
}

impl Collector {
    pub fn new(paths: Arc<PathIndex>, config: ReportConfig) -> Self {
        Self {
            paths,
            config,
            client: reqwest::Client::new(),
            forwarded: CacheBuilder::new(REMEMBERED)
                .time_to_live(WINDOW * 12)
                .build(),
            clients: CacheBuilder::new(CLIENTS).time_to_live(WINDOW * 2).build(),
        }
    }

    pub fn batch_limit(&self) -> usize {
        self.config.max_paths
    }

    /// Whether this client has any of its hourly allowance left.
    pub fn allow(&self, client: &str) -> bool {
        let now = Instant::now();
        let client = client.to_owned();
        let (start, count) = match self.clients.get(&client) {
            Some((start, count)) if now.duration_since(start) < WINDOW => (start, count),
            _ => (now, 0),
        };
        if count >= self.config.per_hour {
            return false;
        }
        self.clients.insert(client, (start, count + 1));
        true
    }

    pub async fn submit(&self, submission: Submission) -> Result<Outcome> {
        let mut outcome = Outcome::default();
        let mut wanted = Vec::new();

        let list = self.paths.master().await?;
        for path in submission.paths {
            let Some(path) = canonical(&path) else {
                outcome.rejected += 1;
                continue;
            };
            if list.contains(&path) || self.forwarded.contains_key(&path) {
                outcome.known += 1;
                continue;
            }
            outcome.new += 1;
            wanted.push(path);
        }

        wanted.sort_unstable();
        wanted.dedup();
        if wanted.is_empty() || !self.config.enabled {
            if !wanted.is_empty() {
                log::info!("Not forwarding {} new paths: {wanted:?}", wanted.len());
            }
            return Ok(outcome);
        }

        for chunk in wanted.chunks(BATCH) {
            self.forward(chunk).await?;
            for path in chunk {
                self.forwarded.insert(path.clone(), ());
            }
        }
        log::info!("Forwarded {} new paths: {wanted:?}", wanted.len());
        outcome.forwarded = true;
        Ok(outcome)
    }

    async fn forward(&self, paths: &[String]) -> Result<()> {
        #[derive(Serialize)]
        struct Upload<'a> {
            #[serde(rename = "Entries")]
            entries: &'a [String],
        }

        let response = self
            .client
            .post(&self.config.forward_url)
            .json(&Upload { entries: paths })
            .send()
            .await?;
        // The receiver answers 202 and nothing else on success; a 200 is what its route returns
        // when it drops the body, so treating any 2xx as accepted would lose the batch silently.
        anyhow::ensure!(
            response.status() == reqwest::StatusCode::ACCEPTED,
            "path upload answered {}",
            response.status()
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::{io::Write, sync::Mutex};

    use actix_web::{App, HttpResponse, HttpServer, web};

    use super::*;
    use crate::config::PathList as PathListConfig;

    fn gzipped(text: &str) -> Vec<u8> {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(text.as_bytes()).unwrap();
        encoder.finish().unwrap()
    }

    /// Stands in for ResLogger: serves a path list, and records what is uploaded to it rather than
    /// telling anyone.
    fn stub(
        list: &'static str,
        uploads: Arc<Mutex<Vec<String>>>,
    ) -> (String, actix_web::dev::Server) {
        let server = HttpServer::new(move || {
            let uploads = uploads.clone();
            App::new()
                .route(
                    "/list",
                    web::get().to(move || async move { HttpResponse::Ok().body(gzipped(list)) }),
                )
                .route(
                    "/upload",
                    web::post().to(move |body: web::Bytes| {
                        let uploads = uploads.clone();
                        async move {
                            uploads
                                .lock()
                                .unwrap()
                                .push(String::from_utf8_lossy(&body).into_owned());
                            HttpResponse::Accepted().finish()
                        }
                    }),
                )
        })
        .workers(1)
        .bind("127.0.0.1:0")
        .unwrap();
        let port = server.addrs()[0].port();
        (format!("http://127.0.0.1:{port}"), server.run())
    }

    fn collector(base: &str, enabled: bool) -> Collector {
        let paths = Arc::new(PathIndex::new(PathListConfig {
            url: format!("{base}/list"),
            extra_urls: Vec::new(),
            ttl_minutes: 60,
            cache_directory: None,
        }));
        Collector::new(
            paths,
            ReportConfig {
                enabled,
                forward_url: format!("{base}/upload"),
                max_paths: 250,
                per_hour: 60,
            },
        )
    }

    #[actix_web::test]
    async fn a_new_name_goes_upstream_and_a_listed_one_does_not() {
        let uploads: Arc<Mutex<Vec<String>>> = Arc::default();
        let (base, server) = stub("ui/uld/known.uld\nui/uld/other.uld\n", uploads.clone());
        let handle = server.handle();
        actix_web::rt::spawn(server);

        let collector = collector(&base, true);
        let outcome = collector
            .submit(Submission {
                paths: vec![
                    "ui/uld/known.uld".into(),
                    "UI/Uld/New.uld".into(),
                    "ui/uld/1f01a2d3".into(),
                    "not a path".into(),
                ],
            })
            .await
            .unwrap();

        assert_eq!(outcome.new, 1);
        assert_eq!(outcome.known, 1);
        assert_eq!(outcome.rejected, 2);
        assert!(outcome.forwarded);
        assert_eq!(
            uploads.lock().unwrap().as_slice(),
            [r#"{"Entries":["ui/uld/new.uld"]}"#]
        );

        // The same name from a second client costs nothing downstream.
        let again = collector
            .submit(Submission {
                paths: vec!["ui/uld/new.uld".into()],
            })
            .await
            .unwrap();
        assert_eq!((again.new, again.known), (0, 1));
        assert_eq!(uploads.lock().unwrap().len(), 1);

        handle.stop(false).await;
    }

    #[actix_web::test]
    async fn nothing_leaves_the_box_while_forwarding_is_off() {
        let uploads: Arc<Mutex<Vec<String>>> = Arc::default();
        let (base, server) = stub("ui/uld/known.uld\n", uploads.clone());
        let handle = server.handle();
        actix_web::rt::spawn(server);

        let collector = collector(&base, false);
        let outcome = collector
            .submit(Submission {
                paths: vec!["ui/uld/new.uld".into()],
            })
            .await
            .unwrap();

        assert_eq!(outcome.new, 1);
        assert!(!outcome.forwarded);
        assert!(uploads.lock().unwrap().is_empty());

        handle.stop(false).await;
    }

    #[test]
    fn only_a_real_game_path_survives() {
        for bad in [
            "ui/uld/foo bar.uld",
            "ui/uld/foo#bar.uld",
            "ui/uld/foo?a=b.uld",
            "notacategory/foo.uld",
            "ui/uld/noextension",
            "ui/uld/1f01a2d3",
            "ui/uld/.uld",
            "ui/uld/foo.",
            "ui/..",
            "ui/../../etc/passwd",
            "ui//foo.uld",
            "ui",
            "",
        ] {
            assert!(canonical(bad).is_none(), "{bad}");
        }
        assert_eq!(
            canonical("  /UI/Uld/Foo-Bar_1.uld/ ").as_deref(),
            Some("ui/uld/foo-bar_1.uld")
        );
        assert!(canonical(&format!("ui/uld/{}.uld", "a".repeat(MAX_LENGTH))).is_none());
    }
}
