//! Claude subscription OAuth: credential loading, PKCE login, and refresh.
//!
//! Token sources, in precedence order:
//! 1. `CLAUDE_CODE_OAUTH_TOKEN` / `CLAUDE_OAUTH_TOKEN` env (never refreshed)
//! 2. The gateway's own session file (tokens minted by the login flow,
//!    and refresh results persisted back)
//! 3. Claude Code's credentials file (`~/.claude/.credentials.json`)
//! 4. macOS Keychain (`Claude Code-credentials`)

use std::{
    path::Path,
    process::Stdio,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use base64::Engine as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::{net::TcpListener, sync::Mutex};

use crate::{
    claude::cloak::CLI_VERSION,
    config::ClaudeConfig,
    error::{GatewayError, Result},
};

/// Claude Code's macOS Keychain service name. Some versions ship the
/// lowercase alias too; the canonical one wins.
pub const KEYCHAIN_SERVICE: &str = "Claude Code-credentials";

/// Claude Code's public OAuth client id.
pub const OAUTH_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
/// OAuth token endpoint. Peer projects report three working hosts
/// (`platform.claude.com`, `console.anthropic.com`, `api.anthropic.com`);
/// this one is byte-for-byte present in the Claude Code binary.
pub const OAUTH_TOKEN_URL: &str = "https://platform.claude.com/v1/oauth/token";
/// OAuth authorization endpoint.
pub const AUTH_URL: &str = "https://claude.ai/oauth/authorize";
/// Local redirect port for the PKCE callback.
pub const CALLBACK_PORT: u16 = 54545;
/// Scopes requested during login.
pub const LOGIN_SCOPES: &str =
    "user:profile user:inference user:sessions:claude_code user:mcp_servers user:file_upload";
/// Scopes used for refresh grants.
pub const REFRESH_SCOPES: &str = "user:inference user:profile";

/// Refresh when the access token expires within this window.
const EXPIRY_BUFFER: Duration = Duration::from_secs(5 * 60);
/// Exponential backoff bounds for failed refreshes.
const REFRESH_BACKOFF_MIN: Duration = Duration::from_secs(60);
const REFRESH_BACKOFF_MAX: Duration = Duration::from_secs(3600);

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OAuthToken {
    #[serde(rename = "accessToken", alias = "access_token")]
    pub access_token: String,
    #[serde(
        rename = "refreshToken",
        alias = "refresh_token",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub refresh_token: Option<String>,
    /// Milliseconds since the Unix epoch (Claude Code uses JS `Date.now()`).
    #[serde(
        rename = "expiresAt",
        alias = "expires_at",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub expires_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(
        rename = "subscriptionType",
        alias = "subscription_type",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub subscription_type: Option<String>,
}

impl OAuthToken {
    /// Milliseconds until expiry; `None` when the token carries no expiry.
    fn expires_in(&self) -> Option<Duration> {
        let expires_at = self.expires_at?;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        Some(Duration::from_millis(
            expires_at.saturating_sub(now.as_millis() as u64),
        ))
    }

    fn expired(&self) -> bool {
        self.expires_in().is_some_and(|d| d < EXPIRY_BUFFER)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    Env,
    Session,
    CredentialsFile,
    #[cfg(target_os = "macos")]
    Keychain,
}

impl Source {
    fn label(self) -> &'static str {
        match self {
            Self::Env => "env",
            Self::Session => "session-file",
            Self::CredentialsFile => "credentials-file",
            Self::Keychain => "keychain",
        }
    }
}

/// Where the login flow's progress stands.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum LoginStatus {
    /// A login flow is running; the browser must complete it.
    Pending { started_at: String },
    /// The last flow failed.
    Failed { error: String },
    /// No flow is running; the gateway has a usable token or not.
    #[serde(skip)]
    Idle,
}

/// Owns the OAuth token lifecycle for the gateway process.
pub struct TokenManager {
    config: ClaudeConfig,
    http: reqwest::Client,
    state: Mutex<Option<(Source, OAuthToken)>>,
    login: Mutex<Option<LoginStatus>>,
    refresh_lock: Mutex<()>,
    backoff_until: Mutex<Option<Instant>>,
    backoff_delay: Mutex<Duration>,
}

impl TokenManager {
    pub fn new(config: ClaudeConfig) -> Self {
        Self {
            config,
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(30))
                .build()
                .expect("token HTTP client builds"),
            state: Mutex::new(None),
            login: Mutex::new(None),
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
        let Some((source, token)) = guard.clone() else {
            return Err(GatewayError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "No Claude credentials found. Run POST /auth/login, or log in with Claude Code \
                 on this machine, or set CLAUDE_CODE_OAUTH_TOKEN.",
            ));
        };
        if !token.expired() {
            return Ok(token.access_token);
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
                "No Claude credentials loaded",
            ));
        };
        let token = self.refresh_locked(&mut guard, source, true).await?;
        Ok(token.access_token)
    }

    /// Refresh under the held state lock: single-flight, backoff, fallback
    /// to the next source when the refresh token is rejected.
    async fn refresh_locked(
        &self,
        guard: &mut Option<(Source, OAuthToken)>,
        source: Source,
        force: bool,
    ) -> Result<OAuthToken> {
        let Some((_, token)) = guard.clone() else {
            return Err(GatewayError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "No Claude credentials loaded",
            ));
        };
        if source == Source::Env {
            return Err(GatewayError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "The CLAUDE_CODE_OAUTH_TOKEN expired. Set a fresh token or run POST /auth/login.",
            ));
        }
        let Some(refresh_token) = token.refresh_token.clone() else {
            return Err(GatewayError::new(
                axum::http::StatusCode::UNAUTHORIZED,
                "The loaded credential has no refresh token. Run POST /auth/login.",
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
                // Keep the refresh token and identity fields when the
                // response omits them.
                let merged = OAuthToken {
                    refresh_token: fresh.refresh_token.or(Some(refresh_token)),
                    scopes: fresh.scopes.or(token.scopes.clone()),
                    subscription_type: fresh.subscription_type.or(token.subscription_type.clone()),
                    ..fresh
                };
                // The store that was refreshed stays canonical: write the
                // rotated tokens back so Claude Code (sharing the store)
                // stays logged in too. The session file is only touched when
                // it already was the source.
                match source {
                    Source::Session => {
                        let _ = save_token(&self.config.session_file, &merged).await;
                    }
                    Source::CredentialsFile => {
                        write_back_credentials_file(&self.config.credentials_file, &merged).await;
                    }
                    #[cfg(target_os = "macos")]
                    Source::Keychain => write_back_keychain(&merged).await,
                    Source::Env => {}
                }
                *guard = Some((source, merged.clone()));
                Ok(merged)
            }
            Err(error) => {
                let delay = { *self.backoff_delay.lock().await };
                *self.backoff_delay.lock().await = (delay * 2).min(REFRESH_BACKOFF_MAX);
                *self.backoff_until.lock().await = Some(Instant::now() + delay);
                if error.message.contains("invalid_grant") {
                    // The refresh token was revoked: drop this source and let
                    // the next load attempt find newer credentials.
                    *guard = None;
                    let _ = tokio::fs::remove_file(&self.config.session_file).await;
                }
                Err(error)
            }
        }
    }

    async fn exchange_refresh(&self, refresh_token: &str) -> Result<OAuthToken> {
        let response = self
            .http
            .post(&self.config.token_url)
            // The token endpoint sits behind Cloudflare; a missing User-Agent
            // is rejected with 403/1010.
            .header(
                "user-agent",
                format!("claude-cli/{CLI_VERSION} (external, cli)"),
            )
            .json(&serde_json::json!({
                "grant_type": "refresh_token",
                "client_id": OAUTH_CLIENT_ID,
                "refresh_token": refresh_token,
                "scope": REFRESH_SCOPES,
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
        serde_json::from_value(body).map_err(|e| {
            GatewayError::new(
                axum::http::StatusCode::BAD_GATEWAY,
                format!("Token refresh returned an unexpected shape: {e}"),
            )
        })
    }

    /// Load the first available credential source.
    async fn load(&self) -> Option<(Source, OAuthToken)> {
        if let Some(token) = &self.config.oauth_token {
            return Some((
                Source::Env,
                OAuthToken {
                    access_token: token.clone(),
                    refresh_token: None,
                    expires_at: None,
                    scopes: None,
                    subscription_type: None,
                },
            ));
        }
        if let Some(token) = read_session(&self.config.session_file).await {
            return Some((Source::Session, token));
        }
        if let Some(token) = read_credentials_file(&self.config.credentials_file).await {
            return Some((Source::CredentialsFile, token));
        }
        #[cfg(target_os = "macos")]
        if self.config.use_keychain
            && let Some(token) = read_keychain().await
        {
            return Some((Source::Keychain, token));
        }
        None
    }

    /// The active credential's description for /healthz and /auth/status.
    pub async fn status(&self) -> serde_json::Value {
        let login = self.login.lock().await;
        let mut status = match &*login {
            Some(state) => serde_json::to_value(state).unwrap_or_default(),
            None => serde_json::json!({"status": "idle"}),
        };
        drop(login);
        let mut guard = self.state.lock().await;
        if guard.is_none() {
            guard.clone_from(&self.load().await);
        }
        match guard.as_ref() {
            Some((source, token)) => {
                status["authenticated"] = serde_json::json!(!token.expired());
                status["source"] = serde_json::json!(source.label());
                status["subscription_type"] =
                    serde_json::json!(token.subscription_type.clone().unwrap_or_default());
                status["expires_at"] = serde_json::json!(token.expires_at);
            }
            None => {
                status["authenticated"] = serde_json::json!(false);
            }
        }
        status
    }

    /// Start the PKCE login flow in the background; see [`login_flow`].
    pub async fn start_login(self: &std::sync::Arc<Self>) -> Result<LoginStatus> {
        let mut login = self.login.lock().await;
        if matches!(&*login, Some(LoginStatus::Pending { .. })) {
            return Ok(login.as_ref().unwrap().clone());
        }
        *login = Some(LoginStatus::Pending {
            started_at: chrono::Utc::now().to_rfc3339(),
        });
        let manager = self.clone();
        tokio::spawn(async move {
            let result = login_flow(&manager.config).await;
            let mut login = manager.login.lock().await;
            match result {
                Ok(_) => {
                    // Drop the cached credential so the next request loads
                    // the freshly minted token.
                    *manager.state.lock().await = None;
                    *login = None;
                }
                Err(error) => {
                    *login = Some(LoginStatus::Failed {
                        error: error.to_string(),
                    });
                }
            }
        });
        Ok(login.as_ref().unwrap().clone())
    }

    /// Forget cached tokens and delete the gateway's session file.
    pub async fn logout(&self) {
        *self.state.lock().await = None;
        let _ = tokio::fs::remove_file(&self.config.session_file).await;
    }
}

/// Run one PKCE authorization-code flow: open the browser, catch the
/// localhost callback, exchange the code, and store the token.
async fn login_flow(config: &ClaudeConfig) -> Result<OAuthToken> {
    let verifier = pkce_verifier();
    let challenge = pkce_challenge(&verifier);
    // Anthropic's authorize page rejects an arbitrary state with "Invalid
    // request format"; Claude Code and known clients set state == verifier.
    let state = verifier.clone();
    let redirect = format!("http://localhost:{CALLBACK_PORT}/callback");
    let url = format!(
        "{AUTH_URL}?client_id={OAUTH_CLIENT_ID}&code=true&code_challenge={challenge}\
         &code_challenge_method=S256&redirect_uri={}&response_type=code&scope={}&state={state}",
        urlencode(&redirect),
        urlencode(LOGIN_SCOPES),
    );
    let listener = TcpListener::bind(("127.0.0.1", CALLBACK_PORT))
        .await
        .map_err(|e| {
            GatewayError::new(
                axum::http::StatusCode::CONFLICT,
                format!("Port {CALLBACK_PORT} is busy; is another login running? ({e})"),
            )
        })?;
    open_browser(&url);
    eprintln!("Open this URL to log in with your Claude subscription:\n  {url}");
    let code = wait_for_code(listener, &state).await?;
    let token = exchange_code(code, &verifier, &state, &redirect).await?;
    save_token(&config.session_file, &token)
        .await
        .map_err(|e| GatewayError::new(axum::http::StatusCode::BAD_GATEWAY, e.to_string()))?;
    Ok(token)
}

async fn wait_for_code(listener: TcpListener, state: &str) -> Result<String> {
    let (mut socket, _) = tokio::time::timeout(Duration::from_secs(10 * 60), listener.accept())
        .await
        .map_err(|_| GatewayError::new(axum::http::StatusCode::REQUEST_TIMEOUT, "Login timed out"))?
        .map_err(|e| GatewayError::new(axum::http::StatusCode::BAD_GATEWAY, e.to_string()))?;
    let (reader, writer) = socket.split();
    read_request_and_extract_code(reader, writer, state).await
}

// The callback handler reads the HTTP request from the raw socket, replies
// with a tiny HTML page, and extracts ?code / ?error.

use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn read_request_and_extract_code(
    mut reader: impl tokio::io::AsyncRead + Unpin,
    mut writer: impl tokio::io::AsyncWrite + Unpin,
    state: &str,
) -> Result<String> {
    let mut buffer = Vec::new();
    loop {
        let mut chunk = [0u8; 4096];
        let read = tokio::time::timeout(Duration::from_secs(30), reader.read(&mut chunk))
            .await
            .map_err(|_| GatewayError::bad_request("Callback read timed out"))?
            .map_err(|e| GatewayError::new(axum::http::StatusCode::BAD_GATEWAY, e.to_string()))?;
        buffer.extend_from_slice(&chunk[..read]);
        if read == 0 || buffer.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let text = String::from_utf8_lossy(&buffer);
    let target = text
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default();
    let query = target.split('?').nth(1).unwrap_or_default();
    let mut code = None;
    let mut error = None;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next().unwrap_or_default();
        let value = parts.next().unwrap_or_default();
        match key {
            "code" => {
                code = Some(
                    percent_encoding::percent_decode_str(value)
                        .decode_utf8_lossy()
                        .into_owned(),
                )
            }
            "error" => {
                error = Some(
                    percent_encoding::percent_decode_str(value)
                        .decode_utf8_lossy()
                        .into_owned(),
                )
            }
            _ => {}
        }
    }
    let (status_line, body): (&str, String) = match (&code, &error) {
        (Some(_), _) => (
            "200 OK",
            "Login complete. You can close this window.".to_owned(),
        ),
        (_, Some(error)) => ("400 Bad Request", format!("Login failed: {error}")),
        _ => (
            "400 Bad Request",
            "Missing ?code in the callback URL.".to_owned(),
        ),
    };
    let _ = writer
        .write_all(
            format!(
                "HTTP/1.1 {status_line}\r\ncontent-type: text/html; charset=utf-8\r\n\
                 connection: close\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .await;
    let _ = writer.flush().await;
    if let Some(error) = error {
        return Err(GatewayError::bad_request(format!("Login failed: {error}")));
    }
    let code =
        code.ok_or_else(|| GatewayError::bad_request("Missing ?code in the callback URL"))?;
    // The state parameter protects against cross-site request forgery; when
    // it mismatches the code is not ours.
    let returned_state = query
        .split('&')
        .find_map(|pair| pair.strip_prefix("state="))
        .unwrap_or_default();
    if returned_state != state {
        return Err(GatewayError::bad_request("OAuth state mismatch"));
    }
    Ok(code)
}

async fn exchange_code(
    code: String,
    verifier: &str,
    state: &str,
    redirect: &str,
) -> Result<OAuthToken> {
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .expect("login HTTP client builds");
    let response = http
        .post(OAUTH_TOKEN_URL)
        .json(&serde_json::json!({
            "grant_type": "authorization_code",
            "client_id": OAUTH_CLIENT_ID,
            "code": code,
            "redirect_uri": redirect,
            "code_verifier": verifier,
            "state": state,
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
            format!("Token exchange failed ({status}): {code} {detail}"),
        ));
    }
    serde_json::from_value(body).map_err(|e| {
        GatewayError::new(
            axum::http::StatusCode::BAD_GATEWAY,
            format!("Token exchange returned an unexpected shape: {e}"),
        )
    })
}

fn open_browser(url: &str) {
    let result = open_browser_command(url);
    let _ = result;
}

#[cfg(target_os = "macos")]
fn open_browser_command(url: &str) -> std::io::Result<std::process::Child> {
    std::process::Command::new("open")
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

#[cfg(all(unix, not(target_os = "macos")))]
fn open_browser_command(url: &str) -> std::io::Result<std::process::Child> {
    std::process::Command::new("xdg-open")
        .arg(url)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

#[cfg(windows)]
fn open_browser_command(url: &str) -> std::io::Result<std::process::Child> {
    std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
}

fn pkce_verifier() -> String {
    // Two v4 UUIDs (122 random bits each) strip to 44 base64url characters,
    // comfortably above the 43-character PKCE minimum.
    let raw = format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    );
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(raw.as_bytes())
}

fn pkce_challenge(verifier: &str) -> String {
    let digest = Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

fn urlencode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

async fn read_session(path: &Path) -> Option<OAuthToken> {
    let bytes = tokio::fs::read(path).await.ok()?;
    serde_json::from_slice(&bytes).ok()
}

async fn read_credentials_file(path: &Path) -> Option<OAuthToken> {
    let bytes = tokio::fs::read(path).await.ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    serde_json::from_value(value["claudeAiOauth"].clone()).ok()
}

#[cfg(target_os = "macos")]
async fn read_keychain() -> Option<OAuthToken> {
    for label in [KEYCHAIN_SERVICE, "claude-code-credentials"] {
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new("security")
                .args(["find-generic-password", "-s", label, "-w"])
                .output(),
        )
        .await
        .ok()?
        .ok()?;
        if !output.status.success() {
            continue;
        }
        let raw = String::from_utf8_lossy(&output.stdout);
        let value: serde_json::Value = serde_json::from_str(raw.trim()).ok()?;
        if let Ok(token) = serde_json::from_value(value["claudeAiOauth"].clone()) {
            return Some(token);
        }
    }
    None
}

/// Persist a token to the gateway's session file (atomic, owner-only).
pub async fn save_token(path: &Path, token: &OAuthToken) -> std::io::Result<()> {
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
            "accessToken": token.access_token,
            "refreshToken": token.refresh_token,
            "expiresAt": token.expires_at,
            "subscriptionType": token.subscription_type,
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

/// Write refreshed tokens back into Claude Code's credentials file,
/// preserving the file's other content. Best-effort: a failure only means
/// Claude Code may need to `/login` again, never breaks the gateway.
async fn write_back_credentials_file(path: &Path, token: &OAuthToken) {
    let Ok(bytes) = tokio::fs::read(path).await else {
        return;
    };
    let Ok(mut value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    value["claudeAiOauth"] = oauth_json(token);
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

/// Write refreshed tokens back into Claude Code's macOS Keychain entry,
/// updating the existing item in place (same mechanism the CLI itself uses).
#[cfg(target_os = "macos")]
async fn write_back_keychain(token: &OAuthToken) {
    let blob = serde_json::json!({ "claudeAiOauth": oauth_json(token) }).to_string();
    let account = keychain_account().await;
    let _ = tokio::process::Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            &account,
            "-w",
            &blob,
        ])
        .output()
        .await;
}

/// The account name of the existing keychain item, so the write-back
/// updates it instead of creating a twin.
#[cfg(target_os = "macos")]
async fn keychain_account() -> String {
    if let Ok(output) = tokio::process::Command::new("security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE])
        .output()
        .await
    {
        let stdout = String::from_utf8_lossy(&output.stdout);
        if let Some(rest) = stdout.split("\"acct\"<blob>=\"").nth(1)
            && let Some(account) = rest.split('"').next()
        {
            return account.to_owned();
        }
    }
    std::env::var("USER").unwrap_or_else(|_| "unknown".to_owned())
}

/// The `claudeAiOauth` object in Claude Code's own JSON shape.
fn oauth_json(token: &OAuthToken) -> serde_json::Value {
    serde_json::json!({
        "accessToken": token.access_token,
        "refreshToken": token.refresh_token,
        "expiresAt": token.expires_at,
        "scopes": token.scopes,
        "subscriptionType": token.subscription_type,
    })
}

/// A stable per-installation device id for `metadata.user_id`, stored in
/// its own file next to the session file (it must not share the token
/// store: Claude Code owns that shape).
pub async fn device_id(session_file: &Path) -> String {
    let path = session_file.with_file_name("device-id");
    if let Ok(bytes) = tokio::fs::read(&path).await {
        let trimmed = String::from_utf8_lossy(&bytes);
        let id = trimmed.trim();
        if !id.is_empty() {
            return id.to_owned();
        }
    }
    let id = uuid::Uuid::new_v4().to_string();
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        let _ = tokio::fs::create_dir_all(parent).await;
    }
    let _ = tokio::fs::write(&path, &id).await;
    id
}

/// The account uuid embedded in `metadata.user_id`. The real account id is
/// not exposed by the OAuth token; a stable namespace-derived value matches
/// what peer proxies send.
pub fn account_uuid() -> String {
    uuid::Uuid::new_v5(&uuid::Uuid::NAMESPACE_OID, b"llm-proxy").to_string()
}
