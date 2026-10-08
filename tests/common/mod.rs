#![allow(dead_code)]

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

use axum::{
    Json, Router,
    body::to_bytes,
    extract::{Request, State},
    http::StatusCode,
    response::{IntoResponse, Response},
};
use claude_codex_api::{
    client::ApiClient,
    config::{AdapterConfig, RestConfig},
    http,
    tools::ToolRegistry,
};
use serde_json::{Value, json};
use tokio::{net::TcpListener, sync::Mutex, task::JoinHandle};

pub struct Server {
    pub base: String,
    task: JoinHandle<()>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn server(router: Router) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await
        .unwrap();
    });
    Server { base, task }
}

#[derive(Default)]
pub struct Fixture {
    pub logins: AtomicUsize,
    pub omit_cookie: AtomicBool,
    pub requests: Mutex<Vec<Value>>,
}

pub async fn upstream() -> (Server, Arc<Fixture>) {
    let fixture = Arc::new(Fixture::default());
    (
        server(Router::new().fallback(echo).with_state(fixture.clone())).await,
        fixture,
    )
}

async fn echo(State(fixture): State<Arc<Fixture>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let method = request.method().to_string();
    let query = request.uri().query().unwrap_or("").to_owned();
    let headers: HashMap<String, String> = request
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
        .collect();
    let bytes = to_bytes(request.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    let record = json!({"method": method, "path": path, "query": query, "headers": headers, "body": body, "body_present": !bytes.is_empty()});
    fixture.requests.lock().await.push(record.clone());
    if path == "/api/v1/auth/login" {
        fixture.logins.fetch_add(1, Ordering::SeqCst);
        return Json(json!({"ok": true})).into_response();
    }
    if path.ends_with("/unauthorized") {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"title": "Unauthorized"})),
        )
            .into_response();
    }
    if path.ends_with("/missing") {
        return (StatusCode::NOT_FOUND, Json(json!({"title": "Not Found"}))).into_response();
    }
    if path.ends_with("/conflict") {
        return (StatusCode::CONFLICT, Json(json!({"title": "Conflict", "detail": "Revision changed", "current": {"revision": 4}}))).into_response();
    }
    if path.ends_with("/empty") {
        return StatusCode::NO_CONTENT.into_response();
    }
    if path.ends_with("/text") {
        return "not JSON".into_response();
    }
    if path.ends_with("/redirect") {
        return (StatusCode::FOUND, [("location", "/echo")]).into_response();
    }
    Json(record).into_response()
}

pub fn rest_config(base: &str) -> AdapterConfig {
    AdapterConfig::Rest(RestConfig {
        base_url: base.into(),
        base_path: String::new(),
        token: None,
        api_key: None,
        api_key_header: "x-api-key".into(),
        headers: HashMap::new(),
    })
}

pub async fn gateway(config: AdapterConfig, token: Option<String>) -> (Server, Arc<ApiClient>) {
    let client = Arc::new(ApiClient::new(config).unwrap());
    let tools = Arc::new(ToolRegistry::new(client.clone()).unwrap());
    (
        server(http::router(client.clone(), tools, token)).await,
        client,
    )
}

pub async fn rpc(base: &str, name: &str, args: Value) -> (StatusCode, Value) {
    let response = reqwest::Client::new()
        .post(format!("{base}/rpc"))
        .json(&json!({"name": name, "args": args}))
        .send()
        .await
        .unwrap();
    (response.status(), response.json().await.unwrap())
}

// ── Claude adapter fixtures ─────────────────────────────────────────────

use claude_codex_api::claude::{auth::TokenManager as ClaudeAuth, proxy::ClaudeProxy};
use claude_codex_api::codex::{auth::TokenManager as CodexAuth, proxy::CodexProxy};
use claude_codex_api::{config::ClaudeConfig, config::CodexConfig, gateway_http};

#[derive(Default)]
pub struct AnthropicFixture {
    pub requests: Mutex<Vec<Value>>,
    /// Bearer tokens rejected with 401.
    pub reject: Mutex<Vec<String>>,
}

/// A mock api.anthropic.com plus token endpoint.
pub async fn anthropic() -> (Server, Arc<AnthropicFixture>) {
    let fixture = Arc::new(AnthropicFixture::default());
    (
        server(
            Router::new()
                .route(
                    "/v1/oauth/token",
                    axum::routing::post(token_endpoint).with_state(fixture.clone()),
                )
                .fallback(anthropic_echo)
                .with_state(fixture.clone()),
        )
        .await,
        fixture,
    )
}

async fn anthropic_echo(
    State(fixture): State<Arc<AnthropicFixture>>,
    request: Request,
) -> Response {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let headers: HashMap<String, String> = request
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
        .collect();
    let bytes = to_bytes(request.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    fixture.requests.lock().await.push(json!({
        "path": path, "query": query, "headers": headers, "body": body,
    }));
    let bearer = headers.get("authorization").cloned().unwrap_or_default();
    if fixture
        .reject
        .lock()
        .await
        .iter()
        .any(|t| bearer == format!("Bearer {t}"))
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"type": "error", "error": {"type": "authentication_error"}})),
        )
            .into_response();
    }
    if path == "/v1/messages/count_tokens" {
        return Json(json!({"input_tokens": 5})).into_response();
    }
    if body["stream"] == json!(true) {
        return (
            [(header::CONTENT_TYPE, "text/event-stream")],
            "event: message_start\ndata: {\"type\":\"message_start\"}\n\n",
        )
            .into_response();
    }
    Json(json!({
        "id": "msg_1",
        "type": "message",
        "role": "assistant",
        "content": [{"type": "text", "text": "Hello"}],
        "model": body["model"],
    }))
    .into_response()
}

async fn token_endpoint(
    State(fixture): State<Arc<AnthropicFixture>>,
    Json(body): Json<Value>,
) -> Response {
    fixture.requests.lock().await.push(json!({
        "path": "/v1/oauth/token", "body": body,
    }));
    if body["refresh_token"] == json!("revoked") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant", "error_description": "revoked"})),
        )
            .into_response();
    }
    Json(json!({
        "access_token": "tok-2",
        "refresh_token": "ref-2",
        "expires_at": 4_102_444_800_000i64,
    }))
    .into_response()
}

use axum::http::header;

/// A claude gateway whose token comes from `session_file`.
pub async fn claude_gateway(
    api: &str,
    token_url: &str,
    session_file: &std::path::Path,
    gateway_token: Option<String>,
) -> Server {
    claude_gateway_with(api, token_url, session_file, session_file, gateway_token).await
}

/// A claude gateway with distinct credentials-file and session-file paths.
pub async fn claude_gateway_with(
    api: &str,
    token_url: &str,
    credentials_file: &std::path::Path,
    session_file: &std::path::Path,
    gateway_token: Option<String>,
) -> Server {
    let config = ClaudeConfig {
        oauth_token: None,
        use_keychain: false,
        credentials_file: credentials_file.to_owned(),
        session_file: session_file.to_owned(),
        api_base: api.into(),
        token_url: token_url.into(),
    };
    let auth = Arc::new(ClaudeAuth::new(config.clone()));
    let proxy = Arc::new(ClaudeProxy::new(config, auth.clone()).unwrap());
    full_gateway(Some(proxy), Some(auth), None, None, gateway_token).await
}

/// A gateway serving both adapters; either side may be omitted.
pub async fn full_gateway(
    claude: Option<Arc<ClaudeProxy>>,
    claude_auth: Option<Arc<ClaudeAuth>>,
    codex: Option<Arc<CodexProxy>>,
    codex_auth: Option<Arc<CodexAuth>>,
    gateway_token: Option<String>,
) -> Server {
    let (claude, claude_auth) = match (claude, claude_auth) {
        (Some(p), Some(a)) => (p, a),
        _ => {
            let config = ClaudeConfig {
                oauth_token: None,
                use_keychain: false,
                credentials_file: std::path::PathBuf::from("/nonexistent"),
                session_file: std::path::PathBuf::from("/nonexistent"),
                api_base: "http://127.0.0.1:1".into(),
                token_url: "http://127.0.0.1:1/token".into(),
            };
            let auth = Arc::new(ClaudeAuth::new(config.clone()));
            (
                Arc::new(ClaudeProxy::new(config, auth.clone()).unwrap()),
                auth,
            )
        }
    };
    let (codex, codex_auth) = match (codex, codex_auth) {
        (Some(p), Some(a)) => (p, a),
        _ => {
            let config = CodexConfig {
                oauth_token: None,
                auth_file: std::path::PathBuf::from("/nonexistent"),
                session_file: std::path::PathBuf::from("/nonexistent"),
                api_base: "http://127.0.0.1:1".into(),
                token_url: "http://127.0.0.1:1/token".into(),
            };
            let auth = Arc::new(CodexAuth::new(config.clone()));
            (
                Arc::new(CodexProxy::new(config, auth.clone()).unwrap()),
                auth,
            )
        }
    };
    server(gateway_http::router(
        claude,
        claude_auth,
        codex,
        codex_auth,
        gateway_token,
    ))
    .await
}

/// A codex gateway (claude side stubbed) against the given mock backend.
pub async fn codex_gateway_with(
    api: &str,
    token_url: &str,
    auth_file: &std::path::Path,
    session_file: &std::path::Path,
    gateway_token: Option<String>,
) -> Server {
    let config = CodexConfig {
        oauth_token: None,
        auth_file: auth_file.to_owned(),
        session_file: session_file.to_owned(),
        api_base: api.into(),
        token_url: token_url.into(),
    };
    let auth = Arc::new(CodexAuth::new(config.clone()));
    let proxy = Arc::new(CodexProxy::new(config, auth.clone()).unwrap());
    full_gateway(None, None, Some(proxy), Some(auth), gateway_token).await
}

/// Write a session file with a live token.
pub fn write_session(path: &std::path::Path, access: &str, refresh: &str) {
    std::fs::write(
        path,
        json!({
            "accessToken": access,
            "refreshToken": refresh,
            "expiresAt": 4_102_444_800_000i64,
        })
        .to_string(),
    )
    .unwrap();
}

// ── Codex adapter fixtures ──────────────────────────────────────────────

use claude_codex_api::codex::auth::Credential;

#[derive(Default)]
pub struct ChatgptFixture {
    pub requests: Mutex<Vec<Value>>,
    /// Bearer tokens rejected with 401.
    pub reject: Mutex<Vec<String>>,
}

/// A mock chatgpt.com/backend-api/codex plus auth.openai.com token endpoint.
pub async fn chatgpt() -> (Server, Arc<ChatgptFixture>) {
    let fixture = Arc::new(ChatgptFixture::default());
    (
        server(
            Router::new()
                .route(
                    "/oauth/token",
                    axum::routing::post(codex_token_endpoint).with_state(fixture.clone()),
                )
                .fallback(codex_echo)
                .with_state(fixture.clone()),
        )
        .await,
        fixture,
    )
}

async fn codex_echo(State(fixture): State<Arc<ChatgptFixture>>, request: Request) -> Response {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let headers: HashMap<String, String> = request
        .headers()
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_str().unwrap().to_owned()))
        .collect();
    let bytes = to_bytes(request.into_body(), 4 * 1024 * 1024)
        .await
        .unwrap();
    let body: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    fixture.requests.lock().await.push(json!({
        "path": path, "query": query, "headers": headers, "body": body,
    }));
    let bearer = headers.get("authorization").cloned().unwrap_or_default();
    if fixture
        .reject
        .lock()
        .await
        .iter()
        .any(|t| bearer == format!("Bearer {t}"))
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": {"message": "bad token"}})),
        )
            .into_response();
    }
    if path == "/codex/models" {
        return Json(json!({"models": [
            {"slug": "gpt-5.5", "display_name": "GPT-5.5"},
            {"slug": "gpt-5.5-codex", "display_name": "GPT-5.5-Codex"}
        ]}))
        .into_response();
    }
    // The backend always answers with SSE.
    let events = [
        r#"event: response.created
data: {"type":"response.created","response":{"id":"resp_1"}}"#,
        r#"event: response.output_text.delta
data: {"type":"response.output_text.delta","delta":"Hello from Codex"}"#,
        r#"event: response.completed
data: {"type":"response.completed","response":{"id":"resp_1","status":"completed","model":"gpt-5.5","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Hello from Codex"}]}],"usage":{"input_tokens":9,"output_tokens":4}}}"#,
    ];
    (
        [(header::CONTENT_TYPE, "text/event-stream")],
        format!("{}\n\n", events.join("\n\n")),
    )
        .into_response()
}

async fn codex_token_endpoint(
    State(fixture): State<Arc<ChatgptFixture>>,
    Json(body): Json<Value>,
) -> Response {
    fixture.requests.lock().await.push(json!({
        "path": "/oauth/token", "body": body,
    }));
    if body["refresh_token"] == json!("revoked") {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant", "error_description": "revoked"})),
        )
            .into_response();
    }
    // access_token carries exp = far future; account_id echoes the request.
    let exp = 4_102_444_800i64;
    let access = format!(
        "header.{}.sig",
        base64_url(
            &json!({"exp": exp, "https://api.openai.com/auth": {"chatgpt_account_id": "acc-1"}})
        )
    );
    Json(json!({
        "access_token": access,
        "refresh_token": "ref-2",
        "account_id": body["refresh_token"].as_str().map(|_| "acc-1".to_owned()),
    }))
    .into_response()
}

fn base64_url(value: &Value) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string().as_bytes())
}

/// Write a Codex CLI-style auth.json.
pub fn write_codex_auth(path: &std::path::Path, access: &str, refresh: &str, account: &str) {
    std::fs::write(
        path,
        json!({
            "auth_mode": "chatgpt",
            "OPENAI_API_KEY": null,
            "tokens": {
                "id_token": "id-jwt",
                "access_token": access,
                "refresh_token": refresh,
                "account_id": account
            },
            "last_refresh": "2026-01-01T00:00:00Z"
        })
        .to_string(),
    )
    .unwrap();
}

/// A JWT access token carrying `exp` (far future).
pub fn codex_access_token(exp: i64) -> String {
    format!("header.{}.sig", base64_url(&json!({"exp": exp})))
}

/// Build a codex Credential for direct TokenManager tests.
pub fn codex_credential(access: &str, refresh: &str, account: &str) -> Credential {
    Credential {
        access_token: access.to_owned(),
        refresh_token: Some(refresh.to_owned()),
        account_id: Some(account.to_owned()),
        expires_at: Some(4_102_444_800),
    }
}
