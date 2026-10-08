//! Forwarding Messages requests to the Anthropic API as Claude Code would:
//! bearer OAuth, fingerprint headers, and the billing cloak on the body.

use std::{sync::Arc, time::Duration};

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::Value;
use uuid::Uuid;

use crate::{
    claude::{
        auth::{TokenManager, account_uuid, device_id},
        cloak::{CLI_VERSION, derive_entrypoint, inject},
    },
    config::ClaudeConfig,
    error::Result,
};

/// Beta features the Claude Code CLI enables. `prompt-caching-2024-07-31`
/// is absent: prompt caching is GA and the API rejects the stale beta.
pub const ANTHROPIC_BETA: &str = "claude-code-20250219,oauth-2025-04-20,interleaved-thinking-2025-05-14,context-management-2025-06-27,advanced-tool-use-2025-11-20,effort-2025-11-24,structured-outputs-2025-12-15";
pub const ANTHROPIC_VERSION: &str = "2023-06-01";

/// A session id stable for the process lifetime, like one CLI run.
fn session_id() -> &'static str {
    use std::sync::OnceLock;
    static SESSION: OnceLock<String> = OnceLock::new();
    SESSION.get_or_init(|| Uuid::new_v4().to_string())
}

pub struct ClaudeProxy {
    config: ClaudeConfig,
    auth: Arc<TokenManager>,
    http: reqwest::Client,
}

impl ClaudeProxy {
    pub fn new(config: ClaudeConfig, auth: Arc<TokenManager>) -> Result<Self> {
        Ok(Self {
            config,
            auth,
            http: reqwest::Client::builder()
                // Streaming responses stay open far longer than a request.
                .timeout(Duration::from_secs(600))
                .build()?,
        })
    }

    /// `POST /v1/messages`: cloak the body, sign it, forward, and pass the
    /// response through (JSON or SSE stream) verbatim.
    pub async fn messages(&self, headers: &HeaderMap, body: Value) -> Response {
        match self.serve_messages(headers, body).await {
            Ok(response) => response,
            Err(error) => error.into_response(),
        }
    }

    async fn serve_messages(&self, headers: &HeaderMap, mut body: Value) -> Result<Response> {
        let stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        let ua = headers
            .get(header::USER_AGENT)
            .and_then(|v| v.to_str().ok());
        let entrypoint = derive_entrypoint(ua);
        inject(
            &mut body,
            &device_id(&self.config.session_file).await,
            &account_uuid(),
            session_id(),
            entrypoint,
        );
        let accept = if stream {
            "text/event-stream"
        } else {
            "application/json"
        };
        self.forward("POST", "/v1/messages", accept, Some(body), true)
            .await
    }

    /// `POST /v1/messages/count_tokens`: no cloak, just auth and headers.
    pub async fn count_tokens(&self, body: Value) -> Response {
        match self
            .forward(
                "POST",
                "/v1/messages/count_tokens",
                "application/json",
                Some(body),
                false,
            )
            .await
        {
            Ok(response) => response,
            Err(error) => error.into_response(),
        }
    }

    /// A minimal Messages probe that only reads the unified rate-limit
    /// headers, mirroring how the CLI reports plan usage.
    pub async fn usage(&self) -> Result<Response> {
        let body = serde_json::json!({
            "model": "claude-haiku-4-5",
            "max_tokens": 1,
            "messages": [{"role": "user", "content": "."}],
        });
        self.forward(
            "POST",
            "/v1/messages",
            "application/json",
            Some(body),
            false,
        )
        .await
    }

    async fn forward(
        &self,
        method: &str,
        path: &str,
        accept: &str,
        body: Option<Value>,
        cloaked: bool,
    ) -> Result<Response> {
        let token = self.auth.token().await?;
        let response = self
            .send(method, path, accept, body.as_ref(), &token, cloaked)
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED && cloaked {
            // The access token may have been revoked server-side; refresh
            // once and retry before giving up.
            if let Ok(token) = self.auth.refresh().await {
                let response = self
                    .send(method, path, accept, body.as_ref(), &token, cloaked)
                    .await?;
                return self.respond(response).await;
            }
        }
        self.respond(response).await
    }

    async fn send(
        &self,
        method: &str,
        path: &str,
        accept: &str,
        body: Option<&Value>,
        token: &str,
        cloaked: bool,
    ) -> Result<reqwest::Response> {
        let url = format!(
            "{}{path}?beta=true",
            self.config.api_base.trim_end_matches('/')
        );
        let mut request = self
            .http
            .request(
                reqwest::Method::from_bytes(method.as_bytes())
                    .expect("caller supplies a valid method"),
                &url,
            )
            .header("authorization", format!("Bearer {token}"))
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("anthropic-beta", ANTHROPIC_BETA)
            .header("accept", accept)
            .header("accept-encoding", "identity")
            .header("x-app", "cli")
            .header(
                "user-agent",
                format!("claude-cli/{CLI_VERSION} (external, cli)"),
            )
            .header("x-claude-code-session-id", session_id())
            .header("x-stainless-lang", "js")
            .header("x-stainless-runtime", "node")
            .header("x-stainless-runtime-version", "22.17.0")
            .header("x-stainless-package-version", CLI_VERSION)
            .header("x-stainless-os", std::env::consts::OS)
            .header("x-stainless-arch", std::env::consts::ARCH)
            .header("x-stainless-retry-count", "0")
            .header("x-stainless-timeout", "600")
            .header("x-client-request-id", Uuid::new_v4().to_string());
        if !cloaked {
            // Plain API-key semantics: allow browser-style direct access.
            request = request.header("anthropic-dangerous-direct-browser-access", "true");
        }
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .json(body);
        }
        Ok(request.send().await?)
    }

    /// Pass the upstream response through: status, headers (minus framing),
    /// and body — streamed for SSE.
    async fn respond(&self, response: reqwest::Response) -> Result<Response> {
        let status = StatusCode::from_u16(response.status().as_u16()).unwrap_or(StatusCode::OK);
        let mut headers = HeaderMap::new();
        for (name, value) in response.headers() {
            if matches!(
                name.as_str(),
                "content-length" | "transfer-encoding" | "content-encoding" | "connection"
            ) {
                continue;
            }
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_str().as_bytes()),
                HeaderValue::from_bytes(value.as_bytes()),
            ) {
                headers.insert(name, value);
            }
        }
        if response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v.as_bytes().starts_with(b"text/event-stream"))
        {
            return Ok((
                status,
                headers,
                axum::body::Body::from_stream(response.bytes_stream()),
            )
                .into_response());
        }
        let bytes = response.bytes().await?;
        Ok((status, headers, axum::body::Body::from(bytes)).into_response())
    }
}

use reqwest::header::HeaderName;
