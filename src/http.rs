use std::{net::SocketAddr, sync::Arc};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{ConnectInfo, Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{
    NAME, VERSION,
    client::ApiClient,
    config::AdapterConfig,
    error::{GatewayError, Result},
    tools::ToolRegistry,
};

#[derive(Clone)]
struct Gateway {
    client: Arc<ApiClient>,
    tools: Arc<ToolRegistry>,
    token: Option<String>,
}

pub fn router(client: Arc<ApiClient>, tools: Arc<ToolRegistry>, token: Option<String>) -> Router {
    Router::new().fallback(handle).with_state(Gateway {
        client,
        tools,
        token,
    })
}

async fn handle(
    State(gateway): State<Gateway>,
    ConnectInfo(remote): ConnectInfo<SocketAddr>,
    req: Request,
) -> Result<Response> {
    let authorized = if let Some(token) = &gateway.token {
        req.headers()
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            == Some(format!("Bearer {token}").as_str())
    } else {
        remote.ip().is_loopback()
            || matches!(remote.ip(), std::net::IpAddr::V6(ip) if ip.to_ipv4_mapped().is_some_and(|v| v.is_loopback()))
    };
    if !authorized {
        return Err(GatewayError::new(
            StatusCode::UNAUTHORIZED,
            "Unauthorized. Set Authorization: Bearer <GATEWAY_TOKEN> or bind to loopback.",
        ));
    }
    let path = req.uri().path().to_owned();
    let method = req.method().clone();
    if path == "/healthz" {
        let config = &gateway.client.config;
        let AdapterConfig::Rest(rest) = config else {
            unreachable!("the REST gateway serves the REST adapter only")
        };
        let mut health = json!({
            "ok": true,
            "name": NAME,
            "version": VERSION,
            "tokenRequired": gateway.token.is_some(),
            "auth": config.auth_mode(),
            "base": rest.base_url.trim_end_matches('/'),
        });
        if !rest.base_path.is_empty() {
            health["base_path"] = json!(rest.base_path);
        }
        return Ok(Json(health).into_response());
    }
    if path == "/tools" && method == Method::GET {
        return Ok(Json(json!({"tools": gateway.tools.tools()})).into_response());
    }
    if path == "/rpc" && method == Method::POST {
        #[derive(Deserialize)]
        struct Rpc {
            name: String,
            #[serde(default = "empty_args")]
            args: Value,
        }
        fn empty_args() -> Value {
            json!({})
        }
        let payload: Rpc = serde_json::from_value(read_json(req).await?.unwrap_or(Value::Null))
            .map_err(|_| {
                GatewayError::bad_request("Expected {\"name\":\"tool_name\",\"args\":{...}}")
            })?;
        let result = gateway.tools.call(&payload.name, payload.args).await?;
        return Ok(Json(json!({"ok": true, "result": result})).into_response());
    }
    let prefix = format!("/{}", gateway.client.config.name());
    if path.starts_with(&format!("{prefix}/")) {
        let upstream_path = format!(
            "{}{}",
            &path[prefix.len()..],
            req.uri()
                .query()
                .map(|q| format!("?{q}"))
                .unwrap_or_default()
        );
        let body = if matches!(method, Method::GET | Method::HEAD) {
            None
        } else {
            read_json(req).await?
        };
        let result = gateway
            .client
            .request(method.as_str(), &upstream_path, None, body.as_ref())
            .await?;
        return Ok(Json(if result.is_null() {
            json!({"ok": true})
        } else {
            result
        })
        .into_response());
    }
    Err(GatewayError::new(
        StatusCode::NOT_FOUND,
        format!("No route {method} {path}. Try /healthz, /tools, /rpc or {prefix}/<api-path>."),
    ))
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
