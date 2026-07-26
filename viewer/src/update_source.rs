use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::{Mutex, mpsc::Sender},
    time::Duration,
};

use serde::Deserialize;
use url::Url;
use velopack::{Error, VelopackAsset, VelopackAssetFeed, bundle::Manifest, sources::UpdateSource};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(12);
const UPDATE_PACKAGE_CHUNK_SIZE: usize = 2 * 1024 * 1024;

#[derive(Clone, Deserialize)]
struct GithubRelease {
    prerelease: bool,
    published_at: Option<String>,
    assets: Vec<GithubReleaseAsset>,
}

#[derive(Clone, Deserialize)]
struct GithubReleaseAsset {
    name: Option<String>,
    browser_download_url: Option<String>,
}

pub(crate) struct GithubUpdateSource {
    mirror_url: Url,
    repository: String,
    metadata_agent: ureq::Agent,
    download_agent: ureq::Agent,
    prefer_direct: bool,
    releases: Mutex<Option<Vec<GithubRelease>>>,
}

impl GithubUpdateSource {
    pub(crate) fn new(repository_url: &str, mirror_url: &str) -> Result<Self, Error> {
        let repository_url = Url::parse(repository_url)?;
        let host = repository_url
            .host_str()
            .ok_or_else(|| Error::Other("GitHub 更新仓库缺少主机名".to_owned()))?;
        if !host.eq_ignore_ascii_case("github.com") {
            return Err(Error::Other(
                "GitHub 更新仓库必须位于 github.com".to_owned(),
            ));
        }

        let repository = repository_url.path().trim_matches('/').to_owned();
        if repository.is_empty() {
            return Err(Error::Other("GitHub 更新仓库路径为空".to_owned()));
        }

        let mut mirror_url = Url::parse(mirror_url)?;
        if mirror_url.scheme() != "https" {
            return Err(Error::Other("GitHub 镜像必须使用 HTTPS".to_owned()));
        }
        mirror_url.set_path("/");
        mirror_url.set_query(None);

        let proxy = ureq::Proxy::try_from_env();
        let prefer_direct = proxy.is_some();
        log::info!(
            "更新网络将优先使用{}",
            if prefer_direct {
                "系统代理访问 GitHub"
            } else {
                "GitHub 镜像"
            }
        );

        Ok(Self {
            mirror_url,
            repository,
            metadata_agent: ureq::Agent::config_builder()
                .proxy(proxy.clone())
                .https_only(true)
                .timeout_connect(Some(CONNECT_TIMEOUT))
                .timeout_recv_response(Some(RESPONSE_TIMEOUT))
                .timeout_global(Some(METADATA_REQUEST_TIMEOUT))
                .build()
                .into(),
            download_agent: ureq::Agent::config_builder()
                .proxy(proxy)
                .https_only(true)
                .timeout_connect(Some(CONNECT_TIMEOUT))
                .timeout_recv_response(Some(RESPONSE_TIMEOUT))
                .build()
                .into(),
            prefer_direct,
            releases: Mutex::new(None),
        })
    }

    fn get_releases(&self) -> Result<Vec<GithubRelease>, Error> {
        if let Some(releases) = self
            .releases
            .lock()
            .map_err(|_| Error::Other("更新缓存状态不可用".to_owned()))?
            .as_ref()
        {
            return Ok(releases.clone());
        }

        let api_url = Url::parse("https://api.github.com/")?
            .join(format!("repos/{}/releases?per_page=10&page=1", self.repository).as_str())?;
        let json = self.download_metadata(&api_url, "application/vnd.github.v3+json")?;
        let mut releases = serde_json::from_str::<Vec<GithubRelease>>(&json)?;
        releases.sort_by(|left, right| right.published_at.cmp(&left.published_at));
        *self
            .releases
            .lock()
            .map_err(|_| Error::Other("更新缓存状态不可用".to_owned()))? = Some(releases.clone());
        Ok(releases)
    }

    fn get_asset_url(&self, release: &GithubRelease, asset_name: &str) -> Result<Url, Error> {
        let asset_url = release
            .assets
            .iter()
            .find(|asset| {
                asset
                    .name
                    .as_deref()
                    .is_some_and(|name| name.eq_ignore_ascii_case(asset_name))
            })
            .and_then(|asset| asset.browser_download_url.as_deref())
            .ok_or_else(|| Error::Other(format!("未找到更新文件 {asset_name}")))?;
        let asset_url = Url::parse(asset_url)?;
        validate_github_url(&asset_url)?;
        Ok(asset_url)
    }

    fn get_release_feed(&self, channel: &str) -> Result<VelopackAssetFeed, Error> {
        let feed_name = format!("releases.{channel}.json");
        let mut assets = Vec::new();
        let mut parsed_feed = false;
        let mut failures = Vec::new();
        for release in self
            .get_releases()?
            .iter()
            .filter(|release| !release.prerelease)
        {
            let Ok(url) = self.get_asset_url(release, &feed_name) else {
                continue;
            };

            match self.download_metadata(&url, "application/octet-stream") {
                Ok(json) => match serde_json::from_str::<VelopackAssetFeed>(&json) {
                    Ok(feed) => {
                        parsed_feed = true;
                        assets.extend(feed.Assets);
                    }
                    Err(error) => {
                        log::debug!("无法解析 GitHub 更新清单: {error}");
                        failures.push(error.to_string());
                    }
                },
                Err(error) => {
                    log::debug!("无法下载 GitHub 更新清单: {error}");
                    failures.push(error.to_string());
                }
            }
        }

        if parsed_feed || failures.is_empty() {
            return Ok(VelopackAssetFeed { Assets: assets });
        }

        Err(Error::Other(format!(
            "更新清单均不可用: {}",
            failures.join("; ")
        )))
    }

    fn download_metadata(&self, remote_url: &Url, accept: &str) -> Result<String, Error> {
        let mut failures = Vec::new();
        for url in self.request_urls(remote_url)? {
            let result = (|| {
                let mut response = self
                    .metadata_agent
                    .get(url.as_str())
                    .header("Accept", accept)
                    .call()?;
                Ok::<_, Error>(response.body_mut().read_to_string()?)
            })();
            match result {
                Ok(body) => {
                    log::debug!("更新元数据请求成功: {url}");
                    return Ok(body);
                }
                Err(error) => {
                    log::debug!("更新元数据请求失败: {url}: {error}");
                    failures.push(error.to_string());
                }
            }
        }

        Err(Error::Other(format!(
            "所有更新地址均请求失败: {}",
            failures.join("; ")
        )))
    }

    fn download_asset(
        &self,
        remote_url: &Url,
        file_path: &Path,
        expected_size: u64,
        progress_sender: Option<Sender<i16>>,
    ) -> Result<(), Error> {
        let mut failures = Vec::new();
        for url in self.request_urls(remote_url)? {
            let result = (|| {
                let (_, body) = self.download_agent.get(url.as_str()).call()?.into_parts();
                let mut file = File::create(file_path)?;
                let mut reader = body.into_reader();
                let mut buffer = vec![0; UPDATE_PACKAGE_CHUNK_SIZE];
                let mut downloaded = 0_u64;
                let mut last_progress = 0;

                loop {
                    let size = reader.read(&mut buffer)?;
                    if size == 0 {
                        break;
                    }

                    file.write_all(&buffer[..size])?;
                    downloaded += size as u64;

                    if expected_size > 0 {
                        let progress =
                            (downloaded.saturating_mul(100) / expected_size).min(100) as i16;
                        if progress > last_progress {
                            last_progress = progress;
                            if let Some(progress_sender) = &progress_sender {
                                let _ = progress_sender.send(progress);
                            }
                        }
                    }
                }

                Ok::<_, Error>(())
            })();
            match result {
                Ok(()) => return Ok(()),
                Err(error) => {
                    log::warn!("更新包下载地址失败, 正在尝试备用地址: {url}: {error}");
                    failures.push(error.to_string());
                    if let Some(progress_sender) = &progress_sender {
                        let _ = progress_sender.send(0);
                    }
                }
            }
        }

        Err(Error::Other(format!(
            "所有更新包地址均下载失败: {}",
            failures.join("; ")
        )))
    }

    fn mirror_url(&self, remote_url: &Url) -> Result<Url, Error> {
        validate_github_url(remote_url)?;
        let host = remote_url.host_str().unwrap();
        let mut mirror_url = self.mirror_url.clone();
        mirror_url.set_path(format!("/{host}{}", remote_url.path()).as_str());
        mirror_url.set_query(remote_url.query());
        Ok(mirror_url)
    }

    fn request_urls(&self, remote_url: &Url) -> Result<[Url; 2], Error> {
        let mirror_url = self.mirror_url(remote_url)?;
        if self.prefer_direct {
            Ok([remote_url.clone(), mirror_url])
        } else {
            Ok([mirror_url, remote_url.clone()])
        }
    }
}

impl UpdateSource for GithubUpdateSource {
    fn get_release_feed(
        &self,
        channel: &str,
        _app: &Manifest,
        _staged_user_id: &str,
    ) -> Result<VelopackAssetFeed, Error> {
        self.get_release_feed(channel)
    }

    fn download_release_entry(
        &self,
        asset: &VelopackAsset,
        local_file: &Path,
        progress_sender: Option<Sender<i16>>,
    ) -> Result<(), Error> {
        for release in self
            .get_releases()?
            .iter()
            .filter(|release| !release.prerelease)
        {
            let Ok(url) = self.get_asset_url(release, &asset.FileName) else {
                continue;
            };

            log::info!("正在下载更新: {url}");
            return self.download_asset(&url, local_file, asset.Size, progress_sender);
        }

        Err(Error::Other(format!("未找到更新文件 {}", asset.FileName)))
    }
}

fn is_github_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("github.com")
        || host.eq_ignore_ascii_case("api.github.com")
        || host.eq_ignore_ascii_case("release-assets.githubusercontent.com")
        || host.ends_with(".githubusercontent.com")
}

fn validate_github_url(url: &Url) -> Result<(), Error> {
    let host = url
        .host_str()
        .ok_or_else(|| Error::Other("GitHub 资源 URL 缺少主机名".to_owned()))?;
    if url.scheme() != "https" || !is_github_host(host) {
        return Err(Error::Other("GitHub 资源 URL 不受信任".to_owned()));
    }
    Ok(())
}
