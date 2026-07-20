use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::mpsc::Sender,
    time::Duration,
};

use serde::Deserialize;
use url::Url;
use velopack::{Error, VelopackAsset, VelopackAssetFeed, bundle::Manifest, sources::UpdateSource};

const METADATA_REQUEST_TIMEOUT: Duration = Duration::from_secs(3);
const UPDATE_PACKAGE_CHUNK_SIZE: usize = 2 * 1024 * 1024;

#[derive(Deserialize)]
struct GithubRelease {
    prerelease: bool,
    published_at: Option<String>,
    assets: Vec<GithubReleaseAsset>,
}

#[derive(Deserialize)]
struct GithubReleaseAsset {
    name: Option<String>,
    browser_download_url: Option<String>,
}

pub(crate) struct GithubMirrorSource {
    mirror_url: Url,
    repository: String,
    metadata_agent: ureq::Agent,
    download_agent: ureq::Agent,
}

impl GithubMirrorSource {
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

        Ok(Self {
            mirror_url,
            repository,
            metadata_agent: ureq::Agent::config_builder()
                .timeout_global(Some(METADATA_REQUEST_TIMEOUT))
                .build()
                .into(),
            download_agent: ureq::Agent::config_builder().build().into(),
        })
    }

    fn get_releases(&self) -> Result<Vec<GithubRelease>, Error> {
        let api_url = Url::parse("https://api.github.com/")?
            .join(format!("repos/{}/releases?per_page=10&page=1", self.repository).as_str())?;
        let url = self.mirror_url(api_url.as_str())?;
        let json = self.download_metadata(&url, "application/vnd.github.v3+json")?;
        let mut releases = serde_json::from_str::<Vec<GithubRelease>>(&json)?;
        releases.sort_by(|left, right| right.published_at.cmp(&left.published_at));
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
        self.mirror_url(asset_url)
    }

    fn get_release_feed(&self, channel: &str) -> Result<VelopackAssetFeed, Error> {
        let feed_name = format!("releases.{channel}.json");
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
                    Ok(feed) => return Ok(feed),
                    Err(error) => log::debug!("无法解析 GitHub 镜像更新清单: {error}"),
                },
                Err(error) => log::debug!("无法下载 GitHub 镜像更新清单: {error}"),
            }
        }

        Ok(VelopackAssetFeed { Assets: Vec::new() })
    }

    fn download_metadata(&self, url: &Url, accept: &str) -> Result<String, Error> {
        let mut response = self
            .metadata_agent
            .get(url.as_str())
            .header("Accept", accept)
            .call()?;
        Ok(response.body_mut().read_to_string()?)
    }

    fn download_asset(
        &self,
        url: &Url,
        file_path: &Path,
        progress_sender: Option<Sender<i16>>,
    ) -> Result<(), Error> {
        let (head, body) = self.download_agent.get(url.as_str()).call()?.into_parts();
        let total_size = head
            .headers
            .get("Content-Length")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.parse::<u64>().ok());
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

            if let (Some(total_size), Some(progress_sender)) = (total_size, &progress_sender) {
                let progress = (downloaded as f64 / total_size as f64 * 20.0).floor() as i16 * 5;
                if progress > last_progress {
                    last_progress = progress;
                    let _ = progress_sender.send(progress);
                }
            }
        }

        Ok(())
    }

    fn mirror_url(&self, remote_url: &str) -> Result<Url, Error> {
        let remote_url = Url::parse(remote_url)?;
        let host = remote_url
            .host_str()
            .ok_or_else(|| Error::Other("GitHub 资源 URL 缺少主机名".to_owned()))?;
        if remote_url.scheme() != "https" || !is_github_host(host) {
            return Err(Error::Other("GitHub 资源 URL 不受信任".to_owned()));
        }

        let mut mirror_url = self.mirror_url.clone();
        mirror_url.set_path(format!("/{host}{}", remote_url.path()).as_str());
        mirror_url.set_query(remote_url.query());
        Ok(mirror_url)
    }
}

impl UpdateSource for GithubMirrorSource {
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

            log::info!("正在从 GitHub 镜像下载更新: {url}");
            return self.download_asset(&url, local_file, progress_sender);
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
