//! HTTP surface for the subscription gateway: the Claude Messages proxy and
//! the Codex Responses proxy, plus login, status, and usage endpoints.

use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

use crate::{
    NAME, VERSION,
    claude::{auth::TokenManager as ClaudeAuth, proxy::ClaudeProxy},
    codex::{auth::TokenManager as CodexAuth, proxy::CodexProxy},
    error::{GatewayError, Result},
};

#[derive(Clone)]
struct Gateway {
    claude: Arc<ClaudeProxy>,
    claude_auth: Arc<ClaudeAuth>,
    codex: Arc<CodexProxy>,
    codex_auth: Arc<CodexAuth>,
    token: Option<String>,
}

pub fn router(
    claude: Arc<ClaudeProxy>,
    claude_auth: Arc<ClaudeAuth>,
    codex: Arc<CodexProxy>,
    codex_auth: Arc<CodexAuth>,
    token: Option<String>,
) -> Router {
    Router::new().fallback(handle).with_state(Gateway {
        claude,
        claude_auth,
        codex,
        codex_auth,
        token,
    })
}

async fn handle(
    State(gateway): State<Gateway>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    req: Request,
) -> Result<Response> {
    authorize(&gateway, remote, req.headers())?;
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    let method_label = method.to_string();
    match (method, path.as_str()) {
        (Method::GET, "/healthz") => Ok(Json(healthz(&gateway)).into_response()),
        (Method::POST, "/v1/messages") => {
            let headers = req.headers().clone();
            let body = read_json(req).await?;
            Ok(gateway
                .claude
                .messages(
                    &headers,
                    body.ok_or_else(|| {
                        GatewayError::bad_request("A JSON request body is required")
                    })?,
                )
                .await)
        }
        (Method::POST, "/v1/messages/count_tokens") => {
            let body = read_json(req).await?;
            Ok(gateway
                .claude
                .count_tokens(
                    body.ok_or_else(|| {
                        GatewayError::bad_request("A JSON request body is required")
                    })?,
                )
                .await)
        }
        (Method::POST, "/v1/responses") => {
            let body = read_json(req).await?;
            Ok(gateway
                .codex
                .responses(
                    body.ok_or_else(|| {
                        GatewayError::bad_request("A JSON request body is required")
                    })?,
                )
                .await)
        }
        (Method::GET, "/v1/models") => Ok(gateway.codex.models().await),
        (Method::GET, "/usage") => Ok(gateway.claude.usage().await?),
        (Method::GET, "/usage/codex") => match gateway.codex.rate_limits() {
            Some(snapshot) => Ok(Json(snapshot).into_response()),
            None => Ok((
                StatusCode::SERVICE_UNAVAILABLE,
                Json(json!({"error": "No Codex request has been served yet; make one first"})),
            )
                .into_response()),
        },
        (Method::POST, "/auth/login") => {
            let status = gateway.claude_auth.start_login().await?;
            Ok(Json(status).into_response())
        }
        (Method::GET, "/auth/status") => Ok(Json(auth_status(&gateway).await).into_response()),
        (Method::POST, "/auth/logout") => {
            gateway.claude_auth.logout().await;
            gateway.codex_auth.logout().await;
            Ok(Json(json!({"ok": true})).into_response())
        }
        _ => Err(GatewayError::new(
            StatusCode::NOT_FOUND,
            format!(
                "No route {method_label} {path}. Try POST /v1/messages, POST /v1/responses, \
                 GET /v1/models, GET /usage, POST /auth/login, GET /auth/status, or GET /healthz."
            ),
        )),
    }
}

fn authorize(gateway: &Gateway, remote: SocketAddr, headers: &axum::http::HeaderMap) -> Result<()> {
    let authorized = if let Some(token) = &gateway.token {
        headers.get("authorization").and_then(|v| v.to_str().ok())
            == Some(format!("Bearer {token}").as_str())
    } else {
        remote.ip().is_loopback()
            || matches!(remote.ip(), std::net::IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some_and(|v| v.is_loopback()))
    };
    if authorized {
        Ok(())
    } else {
        Err(GatewayError::new(
            StatusCode::UNAUTHORIZED,
            "Unauthorized. Set Authorization: Bearer <GATEWAY_TOKEN> or bind to loopback.",
        ))
    }
}

fn healthz(gateway: &Gateway) -> Value {
    json!({
        "ok": true,
        "name": NAME,
        "version": VERSION,
        "adapters": ["claude", "codex"],
        "tokenRequired": gateway.token.is_some(),
    })
}

async fn auth_status(gateway: &Gateway) -> Value {
    json!({
        "claude": gateway.claude_auth.status().await,
        "codex": gateway.codex_auth.status().await,
    })
}

async fn read_json(req: Request) -> Result<Option<Value>> {
    let bytes = to_bytes(req.into_body(), 2 * 1024 * 1024)
        .await
        .map_err(|_| {
            GatewayError::new(
                StatusCode::PAYLOAD_TOO_LARGE,
                "Request body exceeds 2 MiB or could not be read",
            )
        })?;
    if bytes.is_empty() {
        return Ok(None);
    }
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_| GatewayError::bad_request("Invalid JSON request body"))
}
