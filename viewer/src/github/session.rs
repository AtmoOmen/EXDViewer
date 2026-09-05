use anyhow::Result;

use crate::{
    settings::GITHUB_AUTH,
    utils::{PromiseKind, TrackedPromise},
};

use super::{
    GithubAuth, RelayResult, build_auth_start, exchange_code, fetch_client_id, take_relayed_result,
};

/// The signed-in account, and the OAuth dance that produces one. App-wide rather than owned by the
/// pull request dialog: the token raises the API's per-IP rate limit for every call the app makes.
#[derive(Default)]
pub struct GithubSession {
    client_id: Option<String>,
    /// Whether the id has been asked for. A server with sign-in switched off answers with an error,
    /// and the menu calls [`Self::prepare`] every frame it is open.
    asked: bool,
    client_id_promise: Option<TrackedPromise<Result<String>>>,
    /// (PKCE verifier, CSRF state)
    pending: Option<(String, String)>,
    exchange: Option<TrackedPromise<Result<GithubAuth>>>,
    error: Option<String>,
}

/// The token to send with GitHub API calls, if the user has signed in.
pub fn token(ctx: &egui::Context) -> Option<String> {
    GITHUB_AUTH.get(ctx).map(|auth| auth.token)
}

impl GithubSession {
    pub fn login(ctx: &egui::Context) -> Option<String> {
        GITHUB_AUTH.get(ctx).map(|auth| auth.login)
    }

    pub fn signing_in(&self) -> bool {
        self.pending.is_some() || self.exchange.is_some()
    }

    pub fn ready(&self) -> bool {
        self.client_id.is_some()
    }

    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    pub fn sign_out(ctx: &egui::Context) {
        GITHUB_AUTH.set(ctx, None);
    }

    pub fn poll(&mut self, ctx: &egui::Context) {
        if self
            .client_id_promise
            .as_ref()
            .is_some_and(|p| p.try_get().is_some())
        {
            match self.client_id_promise.take().unwrap().block_and_take() {
                Ok(id) => self.client_id = Some(id),
                Err(e) => {
                    log::error!("获取 OAuth 客户端 ID 失败: {e}");
                    self.error = Some(e.to_string());
                }
            }
        }

        if let Some((verifier, state)) = self.pending.clone() {
            ctx.request_repaint_after(std::time::Duration::from_millis(200));
            match take_relayed_result() {
                Some(RelayResult::Code {
                    code,
                    state: got_state,
                }) => {
                    self.pending = None;
                    if got_state == state {
                        self.exchange = Some(TrackedPromise::spawn_local(async move {
                            exchange_code(code, verifier).await
                        }));
                    } else {
                        self.error = Some("登录失败: 状态不匹配".to_string());
                    }
                }
                Some(RelayResult::Error(e)) => {
                    self.pending = None;
                    self.error = Some(e);
                }
                None => {}
            }
        }

        if self
            .exchange
            .as_ref()
            .is_some_and(|p| p.try_get().is_some())
        {
            match self.exchange.take().unwrap().block_and_take() {
                Ok(auth) => {
                    log::info!("GitHub 已以 {} 身份登录", auth.login);
                    self.error = None;
                    GITHUB_AUTH.set(ctx, Some(auth));
                }
                Err(e) => {
                    log::error!("GitHub 登录失败: {e}");
                    self.error = Some(e.to_string());
                }
            }
        }
    }

    pub fn prepare(&mut self) {
        if self.asked {
            return;
        }
        self.asked = true;
        self.client_id_promise = Some(TrackedPromise::spawn_local(async move {
            fetch_client_id().await
        }));
    }

    pub fn begin_login(&mut self, ctx: &egui::Context) {
        self.error = None;
        let Some(client_id) = self.client_id.clone() else {
            // Clicking sign-in is worth another attempt at an id that failed to arrive.
            self.asked = false;
            self.prepare();
            self.error = Some("正在准备登录…请稍后重试".to_string());
            return;
        };
        match build_auth_start(&client_id) {
            Ok(start) => {
                self.pending = Some((start.verifier, start.state));
                ctx.open_url(egui::OpenUrl::new_tab(start.url));
            }
            Err(e) => {
                log::error!("发起 GitHub 登录失败: {e}");
                self.error = Some(e.to_string());
            }
        }
    }
}
