use std::{
    collections::HashMap,
    io::{Read, Write},
    sync::{Arc, LazyLock, Mutex},
    time::{Duration, Instant},
};

use actix_web::{
    HttpRequest, HttpResponse, Result,
    error::{ErrorBadGateway, ErrorBadRequest},
    get, web,
};
use actix_web_lab::header::{CacheControl, CacheDirective};
use bytes::Bytes;
use flate2::{Compression, read::GzDecoder, write::GzEncoder};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::routes::api::accepts;

const API: &str = "https://api.github.com";
/// The unauthenticated API allows 60 calls an hour per IP. Every client sharing one cached answer is
/// the point; five minutes is short enough that a pull request shows up in the picker while it is
/// still worth reviewing, and a revalidation that finds nothing changed costs no quota at all.
const TTL: Duration = Duration::from_secs(5 * 60);
/// A whole repository at a ref, which only moves when a patch does.
const BUNDLE_TTL: Duration = Duration::from_secs(30 * 60);
/// The repository the app opens by default, kept warm so no client ever pays for a cold fetch.
pub const DEFAULT_REPO: (&str, &str) = ("xivdev", "EXDSchema");
pub const DEFAULT_BRANCH: &str = "latest";

pub fn configure(config: &mut web::ServiceConfig) {
    config
        .service(get_branches)
        .service(get_pulls)
        .service(get_pull_files)
        .service(get_schemas);
}

struct Entry {
    fetched: Instant,
    etag: Option<String>,
    body: Bytes,
}

type Store = Mutex<HashMap<String, Arc<Entry>>>;
static JSON: LazyLock<Store> = LazyLock::new(|| Mutex::new(HashMap::new()));
static BUNDLES: LazyLock<Store> = LazyLock::new(|| Mutex::new(HashMap::new()));

static CLIENT: LazyLock<reqwest::Client> = LazyLock::new(reqwest::Client::new);

fn cached(store: &Store, key: &str, ttl: Duration) -> Option<Arc<Entry>> {
    let entry = store.lock().unwrap().get(key).cloned()?;
    (entry.fetched.elapsed() < ttl).then_some(entry)
}

fn store(store: &Store, key: &str, entry: Entry) -> Arc<Entry> {
    let entry = Arc::new(entry);
    store.lock().unwrap().insert(key.to_owned(), entry.clone());
    entry
}

/// GitHub rejects a repository name it would have to escape, and refusing here keeps a crafted one
/// from reaching the upstream URL at all.
fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 100
        && !name.contains("..")
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
}

/// A ref may carry slashes (`ver/7.2`, a fork's head branch) but nothing that would leave the path.
fn valid_ref(name: &str) -> bool {
    !name.is_empty() && name.len() <= 200 && name.split('/').all(valid_name)
}

fn check_repo(owner: &str, repo: &str) -> Result<()> {
    if valid_name(owner) && valid_name(repo) {
        Ok(())
    } else {
        Err(ErrorBadRequest("Invalid GitHub repository name"))
    }
}

/// One API call, revalidated with the etag we already hold. GitHub does not charge rate limit for a
/// 304, so an expired entry usually refreshes for free.
async fn fetch_json(url: &str, known: Option<&Entry>) -> anyhow::Result<Entry> {
    let mut request = CLIENT
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .header("User-Agent", "XIViewer");
    if let Some(etag) = known.and_then(|entry| entry.etag.as_deref()) {
        request = request.header(reqwest::header::IF_NONE_MATCH, etag);
    }

    let response = request.send().await?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_MODIFIED
        && let Some(known) = known
    {
        return Ok(Entry {
            fetched: Instant::now(),
            etag: known.etag.clone(),
            body: known.body.clone(),
        });
    }

    let etag = response
        .headers()
        .get(reqwest::header::ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let body = response.bytes().await?;
    if !status.is_success() {
        anyhow::bail!(
            "GitHub answered {status} for {url}: {}",
            String::from_utf8_lossy(&body)
        );
    }
    Ok(Entry {
        fetched: Instant::now(),
        etag,
        body,
    })
}

/// The cached body for an upstream API call, fetching or revalidating it if it has aged out. A
/// refresh that fails is served stale rather than passed on: an old branch list beats none.
async fn api_json(url: &str) -> Result<Bytes> {
    if let Some(entry) = cached(&JSON, url, TTL) {
        return Ok(entry.body.clone());
    }
    let known = JSON.lock().unwrap().get(url).cloned();
    match fetch_json(url, known.as_deref()).await {
        Ok(entry) => Ok(store(&JSON, url, entry).body.clone()),
        Err(error) => match known {
            Some(stale) => {
                log::warn!("Serving stale {url}: {error}");
                Ok(stale.body.clone())
            }
            None => Err(ErrorBadGateway(error)),
        },
    }
}

fn serve(body: Bytes, max_age: u32) -> HttpResponse {
    HttpResponse::Ok()
        .content_type("application/json")
        .insert_header(CacheControl(vec![
            CacheDirective::Public,
            CacheDirective::MaxAge(max_age),
        ]))
        .body(body)
}

#[get("/github/{owner}/{repo}/branches/")]
async fn get_branches(path: web::Path<(String, String)>) -> Result<HttpResponse> {
    let (owner, repo) = path.into_inner();
    check_repo(&owner, &repo)?;
    let body = api_json(&format!("{API}/repos/{owner}/{repo}/branches?per_page=100")).await?;
    Ok(serve(body, TTL.as_secs() as u32))
}

#[get("/github/{owner}/{repo}/pulls/")]
async fn get_pulls(path: web::Path<(String, String)>) -> Result<HttpResponse> {
    let (owner, repo) = path.into_inner();
    check_repo(&owner, &repo)?;
    let body = api_json(&format!("{API}/repos/{owner}/{repo}/pulls?per_page=100")).await?;
    Ok(serve(body, TTL.as_secs() as u32))
}

#[derive(Debug, Deserialize)]
struct Page {
    page: Option<u32>,
}

#[get("/github/{owner}/{repo}/pulls/{number}/files/")]
async fn get_pull_files(
    path: web::Path<(String, String, u32)>,
    query: web::Query<Page>,
) -> Result<HttpResponse> {
    let (owner, repo, number) = path.into_inner();
    check_repo(&owner, &repo)?;
    let page = query.page.unwrap_or(1).clamp(1, 100);
    let body = api_json(&format!(
        "{API}/repos/{owner}/{repo}/pulls/{number}/files?per_page=100&page={page}"
    ))
    .await?;
    Ok(serve(body, TTL.as_secs() as u32))
}

/// Every schema at a ref in one response. Reading them one at a time is 1200 round trips, which is
/// what listing the sheets that reference an icon costs a client that has to ask GitHub itself.
#[get("/github/{owner}/{repo}/schemas/{git_ref:.*}/")]
async fn get_schemas(
    request: HttpRequest,
    path: web::Path<(String, String, String)>,
) -> Result<HttpResponse> {
    let (owner, repo, git_ref) = path.into_inner();
    check_repo(&owner, &repo)?;
    if !valid_ref(&git_ref) {
        return Err(ErrorBadRequest("Invalid git ref"));
    }

    let body = bundle(&owner, &repo, &git_ref).await?;
    let mut response = HttpResponse::Ok();
    response
        .content_type("application/json")
        .insert_header(CacheControl(vec![
            CacheDirective::Public,
            CacheDirective::MaxAge(BUNDLE_TTL.as_secs() as u32),
        ]))
        .insert_header((actix_web::http::header::VARY, "Accept-Encoding"));

    if accepts(&request, "gzip") {
        return Ok(response
            .insert_header((actix_web::http::header::CONTENT_ENCODING, "gzip"))
            .body(body));
    }
    let mut plain = Vec::new();
    GzDecoder::new(&body[..])
        .read_to_end(&mut plain)
        .map_err(actix_web::error::ErrorInternalServerError)?;
    Ok(response.body(plain))
}

/// Held gzipped, which is both what nearly every caller wants and a twelfth of the size.
pub async fn bundle(owner: &str, repo: &str, git_ref: &str) -> Result<Bytes> {
    let key = format!("{owner}/{repo}@{git_ref}");
    if let Some(entry) = cached(&BUNDLES, &key, BUNDLE_TTL) {
        return Ok(entry.body.clone());
    }

    let url = format!("https://codeload.github.com/{owner}/{repo}/tar.gz/refs/heads/{git_ref}");
    let built = match fetch_bundle(&url).await {
        Ok(built) => built,
        Err(error) => {
            return match BUNDLES.lock().unwrap().get(&key).cloned() {
                Some(stale) => {
                    log::warn!("Serving stale schema bundle {key}: {error}");
                    Ok(stale.body.clone())
                }
                None => Err(ErrorBadGateway(error)),
            };
        }
    };

    log::info!("Bundled schemas for {key} ({} bytes gzipped)", built.len());
    Ok(store(
        &BUNDLES,
        &key,
        Entry {
            fetched: Instant::now(),
            etag: None,
            body: built,
        },
    )
    .body
    .clone())
}

async fn fetch_bundle(url: &str) -> anyhow::Result<Bytes> {
    let response = CLIENT
        .get(url)
        .header("User-Agent", "XIViewer")
        .send()
        .await?
        .error_for_status()?;
    let archive = response.bytes().await?;

    let packed = tokio::task::spawn_blocking(move || {
        let mut schemas = Map::new();
        let mut tar = tar::Archive::new(GzDecoder::new(&archive[..]));
        for entry in tar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            // Schemas sit at the repository root, one per sheet. Anything deeper is the repo's own
            // furniture, and `.github/workflows` is full of yaml that is not a schema.
            let Some(name) = path
                .components()
                .nth(1)
                .filter(|_| path.components().count() == 2)
                .and_then(|c| c.as_os_str().to_str())
                .and_then(|name| name.strip_suffix(".yml"))
            else {
                continue;
            };
            let mut text = String::new();
            entry.read_to_string(&mut text)?;
            schemas.insert(name.to_owned(), Value::String(text));
        }
        anyhow::ensure!(!schemas.is_empty(), "archive carried no schemas");

        let json = serde_json::to_vec(&Value::Object(schemas))?;
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(&json)?;
        anyhow::Ok(Bytes::from(encoder.finish()?))
    })
    .await??;
    Ok(packed)
}

/// Keeps the default repository's answers hot, so the first client of the hour is not the one that
/// waits for GitHub.
pub fn prewarm() {
    tokio::spawn(async {
        let (owner, repo) = DEFAULT_REPO;
        loop {
            for url in [
                format!("{API}/repos/{owner}/{repo}/branches?per_page=100"),
                format!("{API}/repos/{owner}/{repo}/pulls?per_page=100"),
            ] {
                if let Err(error) = api_json(&url).await {
                    log::warn!("Could not prewarm {url}: {error}");
                }
            }
            if let Err(error) = bundle(owner, repo, DEFAULT_BRANCH).await {
                log::warn!("Could not prewarm {owner}/{repo} schemas: {error}");
            }
            tokio::time::sleep(TTL).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Both halves of the upstream URL are built from these, so a name that could steer the path
    /// elsewhere has to be refused before it gets there.
    #[test]
    fn names_that_could_leave_the_repository_are_refused() {
        assert!(valid_name("EXDSchema"));
        assert!(valid_name("xivdev"));
        assert!(valid_name("a.b-c_d"));
        assert!(!valid_name(""));
        assert!(!valid_name("a/b"));
        assert!(!valid_name(".."));
        assert!(!valid_name("a b"));
        assert!(!valid_name("a?b=c"));

        assert!(valid_ref("latest"));
        assert!(valid_ref("ver/7.2"));
        assert!(!valid_ref("../../etc"));
        assert!(!valid_ref("a//b"));
        assert!(!valid_ref(""));
    }
}
