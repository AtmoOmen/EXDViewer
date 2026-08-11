use anyhow::{Result, bail};
use serde::de::DeserializeOwned;

use crate::{
    settings::api_base,
    utils::{HttpResponse, fetch, request},
};

use super::session::token;

pub const API: &str = "https://api.github.com";

/// How the app reaches the GitHub API. The configured server caches these responses so clients do
/// not each spend the per-IP rate limit; asking GitHub directly is what happens when there is no
/// server to ask or it cannot answer, and a signed-in token is what raises the limit for that path.
#[derive(Clone, Default)]
pub struct GithubApi {
    proxy: Option<String>,
    token: Option<String>,
}

impl GithubApi {
    pub fn new(api_url: &str, token: Option<String>) -> Self {
        let proxy = api_url.trim_end_matches('/');
        Self {
            proxy: (!proxy.is_empty()).then(|| proxy.to_owned()),
            token,
        }
    }

    pub fn from_ctx(ctx: &egui::Context) -> Self {
        Self::new(&api_base(ctx), token(ctx))
    }

    /// The cached answer if the server has one, else GitHub's own. `route` is the path below
    /// `/api/github/`; `url` is the equivalent call against GitHub.
    pub async fn get<T: DeserializeOwned>(&self, route: &str, url: &str) -> Result<T> {
        if let Some(proxy) = &self.proxy {
            match server_json(&format!("{proxy}/github/{route}")).await {
                Ok(value) => return Ok(value),
                Err(e) => log::warn!("Falling back to GitHub for {route}: {e}"),
            }
        }
        github_json(url, self.token.as_deref()).await
    }

    /// For what only the server can answer, with no GitHub call to fall back to.
    pub async fn get_from_server<T: DeserializeOwned>(&self, route: &str) -> Result<T> {
        let Some(proxy) = &self.proxy else {
            bail!("No API server is configured");
        };
        server_json(&format!("{proxy}/github/{route}")).await
    }
}

/// Our own server, asked with nothing but the method and the URL. Any header GitHub wants would
/// make this a preflighted request, which a cross-origin API server would then have to allow.
async fn server_json<T: DeserializeOwned>(url: &str) -> Result<T> {
    Ok(serde_json::from_slice(&fetch(url).await?.bytes)?)
}

async fn github_json<T: DeserializeOwned>(url: &str, token: Option<&str>) -> Result<T> {
    let auth = token.map(|token| format!("Bearer {token}"));
    let mut headers = vec![
        ("Accept", "application/vnd.github+json"),
        ("X-GitHub-Api-Version", "2022-11-28"),
        // Ignored by browsers but required on native
        ("User-Agent", "EXDViewer"),
    ];
    if let Some(auth) = &auth {
        headers.push(("Authorization", auth.as_str()));
    }

    let response = request("GET", url, &headers, None).await?;
    if !response.ok {
        bail!(describe(&response, token.is_some()));
    }
    Ok(serde_json::from_slice(&response.bytes)?)
}

/// A used-up rate limit is the one failure the user can do something about, so it says what and
/// names the fix rather than showing GitHub's own body.
fn describe(response: &HttpResponse, authenticated: bool) -> String {
    if is_rate_limited(response) {
        return if authenticated {
            "GitHub's API rate limit for this account is used up. Try again later.".to_string()
        } else {
            "GitHub's API rate limit for your IP is used up. Sign in with GitHub (App menu) to raise it."
                .to_string()
        };
    }
    let text = response.text();
    let message = serde_json::from_str::<serde_json::Value>(&text)
        .ok()
        .as_ref()
        .and_then(|json| json.get("message"))
        .and_then(serde_json::Value::as_str)
        .map_or_else(|| text.clone(), str::to_owned);
    format!("GitHub API request failed ({}): {message}", response.status)
}

fn is_rate_limited(response: &HttpResponse) -> bool {
    if !matches!(response.status, 403 | 429) {
        return false;
    }
    response
        .headers
        .get("x-ratelimit-remaining")
        .is_some_and(|remaining| remaining.trim() == "0")
        || response.text().contains("rate limit")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> HttpResponse {
        HttpResponse {
            status,
            ok: (200..300).contains(&status),
            bytes: body.as_bytes().to_vec(),
            headers: ehttp::Headers::new(headers),
        }
    }

    /// The one failure the user can act on has to be told apart from every other 403, and the
    /// advice has to be worth taking: a signed-in caller is already past the limit sign-in raises.
    #[test]
    fn a_used_up_rate_limit_names_the_fix() {
        let limited = response(
            403,
            &[("x-ratelimit-remaining", "0")],
            r#"{"message":"API rate limit exceeded for 1.2.3.4."}"#,
        );
        assert!(is_rate_limited(&limited));
        assert!(describe(&limited, false).contains("Sign in with GitHub"));
        assert!(!describe(&limited, true).contains("Sign in with GitHub"));

        // GitHub says 429 with no header when it is the secondary limit talking.
        assert!(is_rate_limited(&response(
            429,
            &[],
            r#"{"message":"You have exceeded a secondary rate limit"}"#
        )));

        for other in [
            response(403, &[], r#"{"message":"Must have admin rights"}"#),
            response(404, &[], r#"{"message":"Not Found"}"#),
            response(502, &[], "GitHub answered 403 for ..."),
        ] {
            assert!(!is_rate_limited(&other), "{}", other.text());
        }
    }

    /// GitHub puts the useful half of a failure in `message`; anything else is shown as it came.
    #[test]
    fn a_plain_failure_shows_what_github_said() {
        let message = describe(&response(404, &[], r#"{"message":"Not Found"}"#), false);
        assert_eq!(message, "GitHub API request failed (404): Not Found");
        assert_eq!(
            describe(&response(500, &[], "boom"), false),
            "GitHub API request failed (500): boom"
        );
    }
}
