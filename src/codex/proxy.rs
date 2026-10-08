//! Forwarding Responses requests to the ChatGPT Codex backend as the Codex
//! CLI would: bearer OAuth, account header, and SSE-only transport.

use std::{sync::Arc, time::Duration};

use axum::{
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::Value;

use crate::{
    codex::auth::TokenManager,
    config::CodexConfig,
    error::{GatewayError, Result},
};

/// The Codex CLI's own client identity headers.
pub const ORIGINATOR: &str = "codex_cli_rs";
pub const CLI_VERSION: &str = "0.162.0";
pub const OPENAI_BETA: &str = "responses=experimental";
/// The backend only streams; non-stream clients get the collected final
/// response object instead.
const DEFAULT_INSTRUCTIONS: &str = "You are a helpful assistant.";

/// Last rate-limit snapshot captured from `x-codex-*` response headers.
#[derive(Default)]
pub struct RateLimits(std::sync::Mutex<Option<Value>>);

impl RateLimits {
    fn update(&self, headers: &reqwest::header::HeaderMap) {
        let snapshot = parse_rate_limits(headers);
        if let Ok(mut guard) = self.0.lock()
            && let Some(snapshot) = snapshot
        {
            *guard = Some(snapshot);
        }
    }

    pub fn get(&self) -> Option<Value> {
        self.0.lock().ok().and_then(|guard| guard.clone())
    }
}

/// Parse the `x-codex-*` rate-limit header family into a snapshot, mirroring
/// the CLI's `rate_limits.rs`.
fn parse_rate_limits(headers: &reqwest::header::HeaderMap) -> Option<Value> {
    let get = |name: &str| -> Option<String> {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let primary_used = get("x-codex-primary-used-percent")?;
    let mut snapshot = serde_json::json!({
        "primary": {
            "used_percent": primary_used.parse::<f64>().ok()?,
            "window_minutes": get("x-codex-primary-window-minutes")
                .and_then(|v| v.parse::<u64>().ok()),
            "reset_at": get("x-codex-primary-reset-at")
                .and_then(|v| v.parse::<i64>().ok()),
        }
    });
    if let Some(used) = get("x-codex-secondary-primary-used-percent") {
        snapshot["secondary"] = serde_json::json!({
            "used_percent": used.parse::<f64>().ok()?,
            "window_minutes": get("x-codex-secondary-primary-window-minutes")
                .and_then(|v| v.parse::<u64>().ok()),
            "reset_at": get("x-codex-secondary-primary-reset-at")
                .and_then(|v| v.parse::<i64>().ok()),
        });
    }
    if let Some(reached) = get("x-codex-rate-limit-reached-type") {
        snapshot["rate_limit_reached_type"] = serde_json::json!(reached);
    }
    if let Some(promo) = get("x-codex-promo-message") {
        snapshot["promo_message"] = serde_json::json!(promo);
    }
    Some(snapshot)
}

pub struct CodexProxy {
    config: CodexConfig,
    auth: Arc<TokenManager>,
    http: reqwest::Client,
    rate_limits: Arc<RateLimits>,
}

impl CodexProxy {
    pub fn new(config: CodexConfig, auth: Arc<TokenManager>) -> Result<Self> {
        Ok(Self {
            rate_limits: Arc::new(RateLimits::default()),
            config,
            auth,
            http: reqwest::Client::builder()
                // Streaming responses stay open far longer than a request.
                .timeout(Duration::from_secs(600))
                .build()?,
        })
    }

    /// `POST /v1/responses`: forward verbatim to the Codex backend. Streaming
    /// clients get the SSE stream; non-streaming clients get the final
    /// response object collected from the stream.
    pub async fn responses(&self, body: Value) -> Response {
        match self.serve_responses(body).await {
            Ok(response) => response,
            Err(error) => error.into_response(),
        }
    }

    async fn serve_responses(&self, mut body: Value) -> Result<Response> {
        let client_stream = body.get("stream").and_then(Value::as_bool).unwrap_or(false);
        // The backend only streams, never stores, and rejects empty
        // instructions.
        body["stream"] = Value::Bool(true);
        body["store"] = Value::Bool(false);
        if body
            .get("instructions")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .trim()
            .is_empty()
        {
            body["instructions"] = Value::String(DEFAULT_INSTRUCTIONS.to_owned());
        }
        let response = self
            .forward_raw("POST", "/responses", Some(body), true)
            .await?;
        if client_stream {
            return self.respond(response).await;
        }
        collect_final_response(response).await
    }

    /// `GET /v1/models`: the ChatGPT plan's model catalog.
    pub async fn models(&self) -> Response {
        match self
            .forward_raw("GET", "/models?client_version=1.0.0", None, false)
            .await
        {
            Ok(response) => match self.respond(response).await {
                Ok(response) => response,
                Err(error) => error.into_response(),
            },
            Err(error) => error.into_response(),
        }
    }

    /// The last rate-limit snapshot captured from upstream response headers.
    pub fn rate_limits(&self) -> Option<Value> {
        self.rate_limits.get()
    }

    async fn forward_raw(
        &self,
        method: &str,
        path_and_query: &str,
        body: Option<Value>,
        cloaked: bool,
    ) -> Result<reqwest::Response> {
        let token = self.auth.token().await?;
        let response = self
            .send(method, path_and_query, body.as_ref(), &token)
            .await?;
        if response.status() == StatusCode::UNAUTHORIZED && cloaked {
            // The access token may have been revoked server-side; refresh
            // once and retry before giving up.
            if let Ok(token) = self.auth.refresh().await {
                return self
                    .send(method, path_and_query, body.as_ref(), &token)
                    .await;
            }
        }
        Ok(response)
    }

    async fn send(
        &self,
        method: &str,
        path_and_query: &str,
        body: Option<&Value>,
        token: &str,
    ) -> Result<reqwest::Response> {
        let url = format!(
            "{}{path_and_query}",
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
            .header("chatgpt-account-id", self.account_id().await)
            .header("openai-beta", OPENAI_BETA)
            .header("originator", ORIGINATOR)
            .header(
                "user-agent",
                format!(
                    "{ORIGINATOR}/{CLI_VERSION} ({} {}; {})",
                    std::env::consts::OS,
                    "unknown",
                    std::env::consts::ARCH
                ),
            )
            .header("accept", "text/event-stream")
            .header("accept-encoding", "identity");
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .json(body);
        }
        Ok(request.send().await?)
    }

    async fn account_id(&self) -> String {
        self.auth.account_id().await.unwrap_or_default()
    }

    /// Pass the upstream response through: status, headers (minus framing),
    /// and body — streamed for SSE. Captures `x-codex-*` rate limits.
    async fn respond(&self, response: reqwest::Response) -> Result<Response> {
        self.rate_limits.update(response.headers());
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

/// Consume an SSE stream and return the final `response` object from
/// `response.completed` / `response.incomplete` / `response.failed`.
async fn collect_final_response(response: reqwest::Response) -> Result<axum::response::Response> {
    use futures_util::StreamExt;
    let status = response.status();
    if !status.is_success() {
        let bytes = response.bytes().await?;
        return Ok((
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            axum::body::Body::from(bytes),
        )
            .into_response());
    }
    let mut stream = response.bytes_stream();
    let mut buffer = String::new();
    let mut final_response: Option<Value> = None;
    let mut error: Option<Value> = None;
    'outer: while let Some(chunk) = stream.next().await {
        let bytes = chunk.map_err(|e| GatewayError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
        buffer.push_str(&String::from_utf8_lossy(&bytes));
        // SSE events are separated by blank lines; data lines start "data: ".
        while let Some(index) = buffer.find("\n\n") {
            let event = buffer[..index].to_owned();
            buffer.drain(..index + 2);
            for line in event.lines() {
                let Some(data) = line.strip_prefix("data: ") else {
                    continue;
                };
                let Ok(value) = serde_json::from_str::<Value>(data) else {
                    continue;
                };
                match value["type"].as_str() {
                    Some("response.completed" | "response.incomplete" | "response.done") => {
                        final_response = value.get("response").cloned();
                        break 'outer;
                    }
                    Some("response.failed" | "error") => {
                        error = Some(
                            value
                                .get("response")
                                .and_then(|r| r.get("error"))
                                .cloned()
                                .or_else(|| value.get("error").cloned())
                                .unwrap_or_else(|| value.clone()),
                        );
                        break 'outer;
                    }
                    _ => {}
                }
            }
        }
    }
    if let Some(error) = error {
        return Ok((StatusCode::BAD_GATEWAY, axum::Json(error).into_response()).into_response());
    }
    match final_response {
        Some(response) => Ok(axum::Json(response).into_response()),
        None => Err(GatewayError::new(
            StatusCode::BAD_GATEWAY,
            "The Codex backend stream ended without a final response",
        )),
    }
}
