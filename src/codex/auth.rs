//! Codex (ChatGPT) subscription OAuth: credential loading, refresh, and
//! write-back to the Codex CLI's own `auth.json`.
//!
//! Token sources, in precedence order:
//! 1. `CODEX_OAUTH_TOKEN` env (never refreshed)
//! 2. The gateway's session file (refresh results when the source is ours)
//! 3. Codex CLI's credentials file (`$CODEX_HOME/auth.json`) — refreshed
//!    tokens are written back so the CLI stays logged in.
//!
//! There is no browser login flow here: run `codex login` once, the same
//! hint the CLI itself gives.

use std::{
    path::Path,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::{
    config::CodexConfig,
    error::{GatewayError, Result},
};

/// Codex CLI's public OAuth client id.
pub const OAUTH_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Refresh when the access token expires within this window.
const EXPIRY_BUFFER: Duration = Duration::from_secs(5 * 60);
/// Exponential backoff bounds for failed refreshes.
const REFRESH_BACKOFF_MIN: Duration = Duration::from_secs(60);
const REFRESH_BACKOFF_MAX: Duration = Duration::from_secs(3600);

/// The `tokens` object of Codex CLI's `auth.json`.
#[derive(Clone, Debug, Serialize, Deserialize, Default)]
pub struct Tokens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

#[derive(Clone, Debug)]
pub struct Credential {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub account_id: Option<String>,
    /// Unix seconds; from the JWT `exp` claim.
    pub expires_at: Option<i64>,
}

impl Credential {
    fn expired(&self) -> bool {
        let Some(expires_at) = self.expires_at else {
            return false;
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;
        expires_at - now < EXPIRY_BUFFER.as_secs() as i64
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Env,
    Session,
    AuthFile,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Session => "session-file",
            Self::AuthFile => "auth-file",
        }
    }
}

/// Owns the Codex OAuth token lifecycle for the gateway process.
pub struct TokenManager {
    config: CodexConfig,
    http: reqwest::Client,
    state: Mutex<Option<(Source, Credential)>>,
    refresh_lock: Mutex<()>,
    backoff_until: Mutex<Option<Instant>>,
    backoff_delay: Mutex<Duration>,
}

impl TokenManager {
    pub fn new(config: CodexConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("token HTTP client builds"),
            state: Mutex::new(None),
            refresh_lock: Mutex::new(()),
            backoff_until: Mutex::new(None),
            backoff_delay: Mutex::new(REFRESH_BACKOFF_MIN),
        }
    }

    /// A valid access token, refreshing when it is expired or near expiry.
    pub async fn token(&self) -> Result<String> {
        let mut guard = self.state.lock().await;
        if guard.is_none() {
            guard.clone_from(&self.load().await);
        }
        let Some((source, credential)) = guard.clone() else {
            return Err(GatewayError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "No Codex credentials found. Run `codex login` on this machine, \
                 or set CODEX_OAUTH_TOKEN.",
            ));
        };
        if !credential.expired() {
            return Ok(credential.access_token);
        }
        let refreshed = self.refresh_locked(&mut guard, source, false).await?;
        Ok(refreshed.access_token)
    }

    /// Force a refresh (the upstream rejected the current token with 401).
    pub async fn refresh(&self) -> Result<String> {
        let mut guard = self.state.lock().await;
        let Some((source, _)) = guard.clone() else {
            return Err(GatewayError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "No Codex credentials loaded",
            ));
        };
        let credential = self.refresh_locked(&mut guard, source, true).await?;
        Ok(credential.access_token)
    }

    /// Refresh under the held state lock: single-flight, backoff, and
    /// write-back to the store the credential came from.
    async fn refresh_locked(
        &self,
        guard: &mut Option<(Source, Credential)>,
        source: Source,
        force: bool,
    ) -> Result<Credential> {
        let Some((_, credential)) = guard.clone() else {
            return Err(GatewayError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "No Codex credentials loaded",
            ));
        };
        if source == Source::Env {
            return Err(GatewayError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "The CODEX_OAUTH_TOKEN expired. Set a fresh token or run `codex login`.",
            ));
        }
        let Some(refresh_token) = credential.refresh_token.clone() else {
            return Err(GatewayError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "The loaded credential has no refresh token. Run `codex login`.",
            ));
        };
        {
            let backoff = self.backoff_until.lock().await;
            if let Some(until) = *backoff
                && Instant::now() < until
            {
                return Err(GatewayError::new(
                    axum::http::StatusCode::SERVICE_UNAVAILABLE,
                    "Token refresh is backing off after a recent failure; retry later",
                ));
            }
        }
        let _lock = self.refresh_lock.lock().await;
        // Another task may have refreshed while we waited for the lock; a
        // forced refresh (upstream 401) must always hit the token endpoint.
        if !force
            && let Some((_, current)) = guard.as_ref()
            && !current.expired()
        {
            return Ok(current.clone());
        }
        match self.exchange_refresh(&refresh_token).await {
            Ok(fresh) => {
                *self.backoff_until.lock().await = None;
                *self.backoff_delay.lock().await = REFRESH_BACKOFF_MIN;
                let merged = Credential {
                    refresh_token: Some(fresh.refresh_token.unwrap_or(refresh_token)),
                    account_id: fresh.account_id.or(credential.account_id),
                    ..fresh
                };
                match source {
                    Source::Session => {
                        let _ = save_session(&self.config.session_file, &merged).await;
                    }
                    Source::AuthFile => {
                        write_back_auth_file(&self.config.auth_file, &merged).await;
                    }
                    Source::Env => {}
                }
                *guard = Some((source, merged.clone()));
                Ok(merged)
            }
            Err(error) => {
                let delay = *self.backoff_delay.lock().await;
                *self.backoff_delay.lock().await = (delay * 2).min(REFRESH_BACKOFF_MAX);
                *self.backoff_until.lock().await = Some(Instant::now() + delay);
                if error.message.contains("invalid_grant") {
                    // The refresh token was revoked or already used: drop
                    // this source and let the next load attempt find newer
                    // credentials.
                    *guard = None;
                    let _ = tokio::fs::remove_file(&self.config.session_file).await;
                }
                Err(error)
            }
        }
    }

    async fn exchange_refresh(&self, refresh_token: &str) -> Result<Credential> {
        // The ChatGPT refresh grant uses JSON encoding, unlike the
        // authorization-code grants (see the CLI's oauth client).
        let response = self
            .http
            .post(&self.config.token_url)
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "client_id": OAUTH_CLIENT_ID,
                "refresh_token": refresh_token,
            }))
            .send()
            .await?;
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap_or_default();
        if !status.is_success() {
            let code = body["error"].as_str().unwrap_or("unknown_error");
            let detail = body["error_description"].as_str().unwrap_or("");
            return Err(GatewayError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                format!("Token refresh failed ({status}): {code} {detail}"),
            ));
        }
        let access_token = body["access_token"].as_str().unwrap_or_default();
        if access_token.is_empty() {
            return Err(GatewayError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                "Token refresh returned no access token",
            ));
        }
        Ok(Credential {
            expires_at: jwt_exp(access_token),
            access_token: access_token.to_owned(),
            refresh_token: body["refresh_token"].as_str().map(str::to_owned),
            account_id: body["account_id"].as_str().map(str::to_owned),
        })
    }

    /// Load the first available credential source.
    async fn load(&self) -> Option<(Source, Credential)> {
        if let Some(token) = &self.config.oauth_token {
            return Some((
                Source::Env,
                Credential {
                    access_token: token.clone(),
                    refresh_token: None,
                    account_id: None,
                    expires_at: jwt_exp(token),
                },
            ));
        }
        if let Some(credential) = read_session(&self.config.session_file).await {
            return Some((Source::Session, credential));
        }
        if let Some(credential) = read_auth_file(&self.config.auth_file).await {
            return Some((Source::AuthFile, credential));
        }
        None
    }

    /// The active credential's account id, loading credentials on demand.
    pub async fn account_id(&self) -> Option<String> {
        let mut guard = self.state.lock().await;
        if guard.is_none() {
            guard.clone_from(&self.load().await);
        }
        guard.as_ref().and_then(|(_, c)| c.account_id.clone())
    }

    /// The active credential's description for /auth/status.
    pub async fn status(&self) -> serde_json::Value {
        let mut guard = self.state.lock().await;
        if guard.is_none() {
            guard.clone_from(&self.load().await);
        }
        match guard.as_ref() {
            Some((source, credential)) => {
                serde_json::json!({
                    "authenticated": !credential.expired(),
                    "source": source.label(),
                    "account_id": credential.account_id,
                    "expires_at": credential.expires_at,
                })
            }
            None => serde_json::json!({"authenticated": false}),
        }
    }

    /// Forget cached tokens and delete the gateway's session file.
    pub async fn logout(&self) {
        *self.state.lock().await = None;
        let _ = tokio::fs::remove_file(&self.config.session_file).await;
    }
}

/// Extract the `exp` claim (Unix seconds) from a JWT access token.
fn jwt_exp(token: &str) -> Option<i64> {
    let payload = token.split('.').nth(1)?;
    use base64::Engine as _;
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .ok()?;
    let value: serde_json::Value = serde_json::from_slice(&decoded).ok()?;
    value["exp"].as_i64()
}

async fn read_session(path: &Path) -> Option<Credential> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let tokens: Tokens = serde_json::from_slice(&bytes).ok()?;
    credential_from_tokens(tokens)
}

async fn read_auth_file(path: &Path) -> Option<Credential> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    // API-key mode auth files are not OAuth sessions.
    if value["auth_mode"].as_str().is_some_and(|m| m != "chatgpt") {
        return None;
    }
    let tokens: Tokens = serde_json::from_value(value["tokens"].clone()).ok()?;
    credential_from_tokens(tokens)
}

fn credential_from_tokens(tokens: Tokens) -> Option<Credential> {
    let access_token = tokens.access_token?;
    if access_token.is_empty() {
        return None;
    }
    Some(Credential {
        expires_at: jwt_exp(&access_token),
        access_token,
        refresh_token: tokens.refresh_token.filter(|t| !t.is_empty()),
        account_id: tokens.account_id,
    })
}

/// Persist a credential to the gateway's session file (atomic, owner-only).
async fn save_session(path: &Path, credential: &Credential) -> std::io::Result<()> {
    use tokio::io::AsyncWriteExt;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        tokio::fs::create_dir_all(parent).await?;
    }
    static NEXT_WRITE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let temporary = path.with_extension(format!(
        "{}.{}.tmp",
        std::process::id(),
        NEXT_WRITE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let mut options = tokio::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temporary).await?;
    let result = async {
        let bytes = serde_json::to_vec_pretty(&serde_json::json!({
            "accessToken": credential.access_token,
            "refreshToken": credential.refresh_token,
            "accountId": credential.account_id,
            "expiresAt": credential.expires_at,
            "savedAt": chrono::Utc::now().to_rfc3339(),
        }))?;
        file.write_all(&bytes).await?;
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
    result
}

/// Write refreshed tokens back into the Codex CLI's `auth.json`, preserving
/// the file's other fields and updating `last_refresh` like the CLI does.
/// Best-effort: a failure only means the CLI may need to `codex login` again.
async fn write_back_auth_file(path: &Path, credential: &Credential) {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return;
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    value["tokens"]["access_token"] = serde_json::json!(credential.access_token);
    if let Some(refresh_token) = &credential.refresh_token {
        value["tokens"]["refresh_token"] = serde_json::json!(refresh_token);
    }
    if let Some(account_id) = &credential.account_id {
        value["tokens"]["account_id"] = serde_json::json!(account_id);
    }
    value["last_refresh"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
    let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
    let write = async {
        let mut options = tokio::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options.open(&temporary).await?;
        use tokio::io::AsyncWriteExt;
        file.write_all(&serde_json::to_vec_pretty(&value)?).await?;
        file.flush().await?;
        drop(file);
        tokio::fs::rename(&temporary, path).await
    }
    .await;
    if write.is_err() {
        let _ = tokio::fs::remove_file(&temporary).await;
    }
}
