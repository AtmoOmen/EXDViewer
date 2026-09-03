use std::{cmp::Reverse, collections::HashMap};

use async_trait::async_trait;
use itertools::Itertools;
use serde::Deserialize;
use url::Url;

use crate::{
    github::{API, GithubApi},
    settings::{GithubSchemaBranch, GithubSchemaLocation},
    utils::{GameVersion, fetch_url, fetch_url_str},
};

use super::provider::SchemaProvider;

pub struct WebProvider {
    base_urls: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubCommit {
    pub sha: String,
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubBranch {
    pub name: String,
    pub commit: GithubCommit,
    pub protected: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubPullRequest {
    pub number: u32,
    pub title: String,
    pub user: GithubPullRequestUser,
    pub head: GithubPullRequestHead,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubPullRequestUser {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubPullRequestHead {
    pub label: String,
    pub r#ref: String,
    pub repo: GithubPullRequestRepo,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubPullRequestRepo {
    pub full_name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GithubPullRequestFile {
    pub filename: String,
}

impl WebProvider {
    pub fn new(base_url: String) -> Self {
        WebProvider {
            base_urls: vec![base_url],
        }
    }

    pub fn new_github(location: &GithubSchemaLocation) -> anyhow::Result<Self> {
        let base_url = Url::parse(&location.base_url())?;
        let urls = github_request_urls(&base_url, prefer_direct_github())?;
        Ok(WebProvider {
            base_urls: urls.into_iter().map(Into::into).collect(),
        })
    }

    fn is_valid_github_name(name: &str) -> bool {
        !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    }

    pub async fn fetch_github_repository(
        api: &GithubApi,
        owner: &str,
        repo: &str,
    ) -> anyhow::Result<Vec<GithubSchemaBranch>> {
        if !Self::is_valid_github_name(owner) || !Self::is_valid_github_name(repo) {
            return Err(anyhow::anyhow!("Invalid GitHub repository format"));
        }
let branches: Vec<GithubBranch> = api
            .get(
                &format!("{owner}/{repo}/branches/"),
                &format!("{API}/repos/{owner}/{repo}/branches?per_page=100"),
            )
            .await?;

        let mut ret = Vec::new();
        for branch in branches {
            ret.push(match branch.name.as_str() {
                "latest" => GithubSchemaBranch::Latest,
                "main" | "master" => continue,
                _ => {
                    if let Some(version_string) = branch.name.strip_prefix("ver/")
                        && let Ok(version) = GameVersion::new(version_string)
                    {
                        GithubSchemaBranch::Version(Reverse(version))
                    } else {
                        GithubSchemaBranch::Other(branch.name)
                    }
                }
            });
        }

        Ok(ret)
    }

    pub async fn fetch_github_pull_requests(
        api: &GithubApi,
        owner: &str,
        repo: &str,
    ) -> anyhow::Result<Vec<GithubSchemaBranch>> {
        if !Self::is_valid_github_name(owner) || !Self::is_valid_github_name(repo) {
            return Err(anyhow::anyhow!("Invalid GitHub repository format"));
        }
let pulls: Vec<GithubPullRequest> = api
            .get(
                &format!("{owner}/{repo}/pulls/"),
                &format!("{API}/repos/{owner}/{repo}/pulls?per_page=100"),
            )
            .await?;

        let pulls = pulls
            .into_iter()
            .map(|pull| GithubSchemaBranch::PullRequest {
                number: pull.number,
                title: pull.title,
                label: pull.head.label,
                username: pull.user.login,
                full_name: pull.head.repo.full_name,
                branch: pull.head.r#ref,
            })
            .collect_vec();

        Ok(pulls)
    }

    pub async fn fetch_github_pull_request_files(
        api: &GithubApi,
        owner: &str,
        repo: &str,
        number: u32,
    ) -> anyhow::Result<Vec<String>> {
        if !Self::is_valid_github_name(owner) || !Self::is_valid_github_name(repo) {
            return Err(anyhow::anyhow!("Invalid GitHub repository format"));
        }

        const PER_PAGE: usize = 100;
        let mut ret = Vec::new();
        let mut page = 1u32;
        loop {
let files: Vec<GithubPullRequestFile> = api
                .get(
                    &format!("{owner}/{repo}/pulls/{number}/files/?page={page}"),
                    &format!(
                        "{API}/repos/{owner}/{repo}/pulls/{number}/files?per_page={PER_PAGE}&page={page}"
                    ),
                )
                .await?;
            let count = files.len();

            ret.extend(files.into_iter().filter_map(|file| {
                file.filename
                    .strip_suffix(".yml")
                    .map(|name| name.to_string())
            }));

            if count < PER_PAGE {
                break;
            }
            page += 1;
        }

        Ok(ret)
    }

    /// Every schema at a ref in one response, which only the server can answer: reading them from
    /// GitHub one file at a time is around 1200 round trips.
    pub async fn fetch_github_schemas(
        api: &GithubApi,
        location: &GithubSchemaLocation,
    ) -> anyhow::Result<HashMap<String, String>> {
        let (owner, repo, branch) = location.source();
        if !Self::is_valid_github_name(owner) || !Self::is_valid_github_name(repo) {
            return Err(anyhow::anyhow!("Invalid GitHub repository format"));
        }
        api.get_from_server(&format!("{owner}/{repo}/schemas/{branch}/"))
            .await
    }
}

#[async_trait(?Send)]
impl SchemaProvider for WebProvider {
    async fn get_schema_text(&self, name: &str) -> anyhow::Result<String> {
        let mut failures = Vec::new();
        for base_url in &self.base_urls {
            let url = format!("{}/{name}.yml", base_url.trim_end_matches('/'));
            match fetch_url_str(&url).await {
                Ok(text) => return Ok(text),
                Err(error) => {
                    log::debug!("表定义下载失败, 正在尝试备用地址: {url}: {error}");
                    failures.push(error.to_string());
                }
            }
        }

        anyhow::bail!("所有表定义地址均下载失败: {}", failures.join("; "))
    }

    fn can_save_schemas(&self) -> bool {
        false
    }

    fn save_schema_start_dir(&self) -> Option<std::path::PathBuf> {
        None
    }

    async fn save_schema(&self, _name: &str, _text: &str) -> anyhow::Result<()> {
        unreachable!("Saving schemas is not supported by this provider");
    }
}

fn github_request_urls(remote_url: &Url, prefer_direct: bool) -> anyhow::Result<[Url; 2]> {
    let host = remote_url
        .host_str()
        .ok_or_else(|| anyhow::anyhow!("GitHub URL 缺少主机名"))?;
    if remote_url.scheme() != "https"
        || !(host.eq_ignore_ascii_case("api.github.com")
            || host.eq_ignore_ascii_case("raw.githubusercontent.com"))
    {
        anyhow::bail!("GitHub URL 不受信任")
    }

    let mut mirror_url = Url::parse(crate::GITHUB_MIRROR_URL)?;
    mirror_url.set_path(&format!("/{host}{}", remote_url.path()));
    mirror_url.set_query(remote_url.query());
    if prefer_direct {
        Ok([remote_url.clone(), mirror_url])
    } else {
        Ok([mirror_url, remote_url.clone()])
    }
}

async fn fetch_github_url(remote_url: &Url) -> anyhow::Result<Vec<u8>> {
    let mut failures = Vec::new();
    for url in github_request_urls(remote_url, prefer_direct_github())? {
        match fetch_url(url.as_str()).await {
            Ok(bytes) => return Ok(bytes),
            Err(error) => {
                log::debug!("GitHub 请求失败, 正在尝试备用地址: {url}: {error}");
                failures.push(error.to_string());
            }
        }
    }

    anyhow::bail!("所有 GitHub 地址均请求失败: {}", failures.join("; "))
}

fn prefer_direct_github() -> bool {
    #[cfg(not(target_arch = "wasm32"))]
    {
        ureq::Proxy::try_from_env().is_some()
    }
    #[cfg(target_arch = "wasm32")]
    {
        false
    }
}
