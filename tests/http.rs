mod common;

use std::sync::Arc;

use axum::http::StatusCode;
use claude_codex_api::config::{AdapterConfig, ClaudeConfig, CodexConfig};
use common::*;
use serde_json::{Value, json};

#[tokio::test]
async fn rest_tools_auth_query_bodies_and_passthrough() {
    let (upstream, _) = upstream().await;
    let mut config = rest_config(&upstream.base);
    if let AdapterConfig::Rest(c) = &mut config {
        c.token = Some("t0k".into());
        c.base_path = "/v1".into();
        c.headers.insert("X-Team".into(), "studio".into());
        c.headers.insert("Content-Type".into(), "text/plain".into());
        c.headers
            .insert("Authorization".into(), "overridden".into());
    }
    let (gateway, _) = gateway(config, None).await;
    let http = reqwest::Client::new();
    let health: Value = http
        .get(format!("{}/healthz", gateway.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["auth"], "bearer");
    assert_eq!(health["tokenRequired"], false);
    let tools: Value = http
        .get(format!("{}/tools", gateway.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tools["tools"].as_array().unwrap().len(), 3);
    assert_eq!(
        tools["tools"][0]["inputSchema"]["properties"]["method"]["enum"],
        json!(["GET", "POST", "PATCH", "PUT", "DELETE"])
    );

    let (status, result) = rpc(
        &gateway.base,
        "rest_get",
        json!({"path": "/echo?page=1", "query": {"page": "2", "q": "a b"}}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["result"]["path"], "/v1/echo");
    assert_eq!(result["result"]["headers"]["authorization"], "Bearer t0k");
    assert_eq!(result["result"]["headers"]["x-team"], "studio");
    assert_eq!(result["result"]["query"], "page=2&q=a+b");

    for body in [
        json!({"hello": "world"}),
        json!([1, 2]),
        json!(false),
        json!(12),
        json!("text"),
        Value::Null,
    ] {
        let (status, result) = rpc(
            &gateway.base,
            "rest_post",
            json!({"path": "/echo", "body": body}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["result"]["body"], body);
        assert_eq!(result["result"]["body_present"], true);
        assert_eq!(
            result["result"]["headers"]["content-type"],
            "application/json"
        );
    }
    for method in ["GET", "POST", "PATCH", "PUT", "DELETE"] {
        let (status, result) = rpc(
            &gateway.base,
            "rest_request",
            json!({"method": method, "path": "/echo"}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result["result"]["method"], method);
        assert_eq!(result["result"]["body_present"], false);
    }
    let response: Value = http
        .patch(format!("{}/rest/echo?tag=a&tag=b", gateway.base))
        .json(&json!({"x": 1}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["path"], "/v1/echo");
    assert_eq!(response["query"], "tag=a&tag=b");
    assert_eq!(response["body"], json!({"x": 1}));
}

#[tokio::test]
async fn api_key_and_sessionless_rest() {
    let (upstream, _) = upstream().await;
    let mut config = rest_config(&upstream.base);
    let client = claude_codex_api::client::ApiClient::new(config.clone()).unwrap();
    let result = client.request("GET", "/echo", None, None).await.unwrap();
    assert!(result["headers"].get("cookie").is_none());
    assert!(result["headers"].get("authorization").is_none());
    if let AdapterConfig::Rest(c) = &mut config {
        c.api_key = Some("key".into());
        c.api_key_header = "X-Custom-Key".into();
    }
    let client = claude_codex_api::client::ApiClient::new(config).unwrap();
    let result = client.request("GET", "/echo", None, None).await.unwrap();
    assert_eq!(result["headers"]["x-custom-key"], "key");
}

#[tokio::test]
async fn http_auth_validation_and_error_mapping() {
    let (upstream, _) = upstream().await;
    let (protected, _) = gateway(rest_config(&upstream.base), Some("gateway-secret".into())).await;
    let http = reqwest::Client::new();
    assert_eq!(
        http.get(format!("{}/healthz", protected.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        http.get(format!("{}/healthz", protected.base))
            .bearer_auth("gateway-secret")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let (gateway, _) = gateway(rest_config(&upstream.base), None).await;
    for (name, args) in [
        ("unknown", json!({})),
        ("rest_get", json!({})),
        ("rest_get", json!({"path": "https://evil.example"})),
        ("rest_get", json!({"path": "/echo", "query": {"x": 2}})),
        ("rest_request", json!({"method": "BAD", "path": "/echo"})),
    ] {
        assert_eq!(
            rpc(&gateway.base, name, args).await.0,
            StatusCode::BAD_REQUEST
        );
    }
    for body in ["{", "null", "[]", "{}", ""] {
        let response = http
            .post(format!("{}/rpc", gateway.base))
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert!(response.json::<Value>().await.unwrap()["error"].is_string());
    }
    assert_eq!(
        http.post(format!("{}/rpc", gateway.base))
            .body("x".repeat(2 * 1024 * 1024 + 1))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    let (status, error) = rpc(&gateway.base, "rest_get", json!({"path": "/conflict"})).await;
    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(error["current"], json!({"revision": 4}));
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("Revision changed")
    );
    assert_eq!(
        rpc(&gateway.base, "rest_get", json!({"path": "/missing"}))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        rpc(&gateway.base, "rest_get", json!({"path": "/text"}))
            .await
            .0,
        StatusCode::BAD_GATEWAY
    );
    assert_eq!(
        rpc(&gateway.base, "rest_get", json!({"path": "/empty"}))
            .await
            .1,
        json!({"ok": true, "result": null})
    );
    assert_eq!(
        http.get(format!("{}/rest/empty", gateway.base))
            .send()
            .await
            .unwrap()
            .json::<Value>()
            .await
            .unwrap(),
        json!({"ok": true})
    );
    assert_eq!(
        http.get(format!("{}/unknown", gateway.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
}

// ── Claude adapter ──────────────────────────────────────────────────────

#[tokio::test]
async fn claude_messages_proxy_signs_and_cloaks() {
    let (api, fixture) = anthropic().await;
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("claude-session.json");
    write_session(&session, "tok-1", "ref-1");
    let gateway = claude_gateway(&api.base, "http://unused/token", &session, None).await;
    let http = reqwest::Client::new();
    let response: Value = http
        .post(format!("{}/v1/messages", gateway.base))
        .json(&json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hello world"}],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["content"][0]["text"], "Hello");
    let upstream = fixture.requests.lock().await.pop().unwrap();
    assert_eq!(upstream["path"], "/v1/messages");
    assert_eq!(upstream["query"], "beta=true");
    assert_eq!(upstream["headers"]["authorization"], "Bearer tok-1");
    assert_eq!(upstream["headers"]["anthropic-version"], "2023-06-01");
    assert!(
        upstream["headers"]["anthropic-beta"]
            .as_str()
            .unwrap()
            .contains("claude-code-20250219")
    );
    assert_eq!(upstream["headers"]["x-app"], "cli");
    assert!(
        upstream["headers"]["user-agent"]
            .as_str()
            .unwrap()
            .starts_with("claude-cli/")
    );
    assert!(upstream["headers"]["x-stainless-os"].is_string());
    assert!(upstream["headers"]["x-claude-code-session-id"].is_string());
    let system = upstream["body"]["system"].as_array().unwrap();
    assert_eq!(system.len(), 2);
    assert!(
        system[0]["text"]
            .as_str()
            .unwrap()
            .contains("x-anthropic-billing-header: cc_version=")
    );
    assert!(
        system[0]["text"]
            .as_str()
            .unwrap()
            .contains("cc_entrypoint=cli;")
    );
    assert_eq!(
        system[1]["text"],
        "You are Claude Code, Anthropic's official CLI for Claude."
    );
    assert!(upstream["body"]["metadata"]["user_id"].is_string());
}

#[tokio::test]
async fn claude_streaming_passthrough() {
    let (api, _) = anthropic().await;
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("claude-session.json");
    write_session(&session, "tok-1", "ref-1");
    let gateway = claude_gateway(&api.base, "http://unused/token", &session, None).await;
    let response = reqwest::Client::new()
        .post(format!("{}/v1/messages", gateway.base))
        .json(&json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 16,
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}],
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    let body = response.text().await.unwrap();
    assert!(body.contains("message_start"));
}

#[tokio::test]
async fn claude_count_tokens_usage_and_401_refresh() {
    let (api, fixture) = anthropic().await;
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("claude-session.json");
    write_session(&session, "tok-1", "ref-1");
    let gateway = claude_gateway(
        &api.base,
        &format!("{}/v1/oauth/token", api.base),
        &session,
        None,
    )
    .await;
    let http = reqwest::Client::new();

    let count: Value = http
        .post(format!("{}/v1/messages/count_tokens", gateway.base))
        .json(&json!({"model": "claude-sonnet-4-5", "messages": []}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(count["input_tokens"], 5);

    let usage: Value = http
        .get(format!("{}/usage", gateway.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(usage["content"][0]["text"], "Hello");

    // A 401 from upstream triggers one refresh and retry.
    fixture.reject.lock().await.push("tok-1".into());
    let response: Value = http
        .post(format!("{}/v1/messages", gateway.base))
        .json(&json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["content"][0]["text"], "Hello");
    let requests = fixture.requests.lock().await;
    let refresh = requests
        .iter()
        .find(|r| r["path"] == "/v1/oauth/token")
        .expect("refresh happened");
    assert_eq!(refresh["body"]["grant_type"], "refresh_token");
    assert_eq!(
        refresh["body"]["client_id"],
        "9d1c250a-e61b-44d9-88ed-5944d1962f5e"
    );
    // The session file now holds the refreshed token.
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&session).unwrap()).unwrap();
    assert_eq!(stored["accessToken"], "tok-2");
    assert_eq!(stored["refreshToken"], "ref-2");
}

#[tokio::test]
async fn claude_gateway_token_auth_and_status() {
    let (api, _) = anthropic().await;
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("claude-session.json");
    write_session(&session, "tok-1", "ref-1");
    let gateway = claude_gateway(
        &api.base,
        "http://unused/token",
        &session,
        Some("secret".into()),
    )
    .await;
    let http = reqwest::Client::new();
    assert_eq!(
        http.get(format!("{}/healthz", gateway.base))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNAUTHORIZED
    );
    let health: Value = http
        .get(format!("{}/healthz", gateway.base))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["adapters"], json!(["claude", "codex"]));
    let status: Value = http
        .get(format!("{}/auth/status", gateway.base))
        .bearer_auth("secret")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["claude"]["authenticated"], true);
    assert_eq!(status["claude"]["source"], "session-file");
}

#[tokio::test]
async fn claude_no_credentials_returns_401() {
    let (api, _) = anthropic().await;
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("missing.json");
    let gateway = claude_gateway(&api.base, "http://unused/token", &session, None).await;
    let response = reqwest::Client::new()
        .post(format!("{}/v1/messages", gateway.base))
        .json(&json!({"model": "claude-sonnet-4-5", "messages": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let error: Value = response.json().await.unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("No Claude credentials")
    );
}

#[tokio::test]
async fn claude_refresh_writes_back_to_credentials_file() {
    let (api, fixture) = anthropic().await;
    let dir = tempfile::tempdir().unwrap();
    let credentials = dir.path().join(".credentials.json");
    // Claude Code's own shape, expired, with identity fields to preserve.
    std::fs::write(
        &credentials,
        json!({
            "claudeAiOauth": {
                "accessToken": "tok-1",
                "refreshToken": "ref-1",
                "expiresAt": 1_000i64,
                "scopes": ["user:inference", "user:profile"],
                "subscriptionType": "max"
            },
            "primaryApiKey": "sk-ant-api03-untouched"
        })
        .to_string(),
    )
    .unwrap();
    let session = dir.path().join("claude-session.json");
    let gateway = claude_gateway_with(
        &api.base,
        &format!("{}/v1/oauth/token", api.base),
        &credentials,
        &session,
        None,
    )
    .await;

    fixture.reject.lock().await.push("tok-1".into());
    let response: Value = reqwest::Client::new()
        .post(format!("{}/v1/messages", gateway.base))
        .json(&json!({
            "model": "claude-sonnet-4-5",
            "max_tokens": 16,
            "messages": [{"role": "user", "content": "hi"}],
        }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(response["content"][0]["text"], "Hello");

    // The credentials file carries the rotated token and keeps its other keys.
    let stored: Value =
        serde_json::from_str(&std::fs::read_to_string(&credentials).unwrap()).unwrap();
    assert_eq!(stored["claudeAiOauth"]["accessToken"], "tok-2");
    assert_eq!(stored["claudeAiOauth"]["refreshToken"], "ref-2");
    assert_eq!(stored["claudeAiOauth"]["subscriptionType"], "max");
    assert_eq!(
        stored["claudeAiOauth"]["scopes"],
        json!(["user:inference", "user:profile"])
    );
    assert_eq!(stored["primaryApiKey"], "sk-ant-api03-untouched");
    // The session file was not created: the CLI's store stays canonical.
    assert!(!session.exists());
}

// ── Codex adapter ───────────────────────────────────────────────────────

#[tokio::test]
async fn codex_responses_passthrough_streams_and_collects() {
    let (_api, _fixture) = anthropic().await; // unused claude mock
    let (chatgpt, cg) = chatgpt().await;
    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("auth.json");
    write_codex_auth(&auth, &codex_access_token(4_102_444_800), "ref-1", "acc-1");
    let session = dir.path().join("codex-session.json");
    let gateway = codex_gateway_with(
        &format!("{}/codex", chatgpt.base),
        &format!("{}/oauth/token", chatgpt.base),
        &auth,
        &session,
        None,
    )
    .await;
    let http = reqwest::Client::new();

    // Streaming passthrough.
    let response = http
        .post(format!("{}/v1/responses", gateway.base))
        .json(&json!({"model": "gpt-5.5", "stream": true, "input": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.headers().get("content-type").unwrap(),
        "text/event-stream"
    );
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("response.completed")
    );

    // Non-stream collect.
    let collected: Value = http
        .post(format!("{}/v1/responses", gateway.base))
        .json(&json!({"model": "gpt-5.5", "input": []}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(collected["id"], "resp_1");
    assert_eq!(
        collected["output"][0]["content"][0]["text"],
        "Hello from Codex"
    );

    // Wire checks on the last upstream request.
    let requests = cg.requests.lock().await;
    let last = requests.last().unwrap();
    assert_eq!(last["path"], "/codex/responses");
    assert_eq!(
        last["headers"]["authorization"],
        "Bearer header.eyJleHAiOjQxMDI0NDQ4MDB9.sig"
    );
    assert_eq!(last["headers"]["chatgpt-account-id"], "acc-1");
    assert_eq!(last["headers"]["openai-beta"], "responses=experimental");
    assert_eq!(last["headers"]["originator"], "codex_cli_rs");
    assert!(
        last["headers"]["user-agent"]
            .as_str()
            .unwrap()
            .starts_with("codex_cli_rs/")
    );
    assert_eq!(last["body"]["stream"], json!(true));
    assert_eq!(last["body"]["store"], json!(false));
    assert_eq!(last["body"]["instructions"], "You are a helpful assistant.");
}

#[tokio::test]
async fn codex_models_and_rate_limits() {
    let (chatgpt, cg) = chatgpt().await;
    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("auth.json");
    write_codex_auth(&auth, &codex_access_token(4_102_444_800), "ref-1", "acc-1");
    let session = dir.path().join("codex-session.json");
    let gateway = codex_gateway_with(
        &format!("{}/codex", chatgpt.base),
        &format!("{}/oauth/token", chatgpt.base),
        &auth,
        &session,
        None,
    )
    .await;
    let http = reqwest::Client::new();
    let models: Value = http
        .get(format!("{}/v1/models", gateway.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(models["models"][0]["slug"], "gpt-5.5");
    let requests = cg.requests.lock().await;
    let request = requests.last().unwrap();
    assert_eq!(request["path"], "/codex/models");
    assert_eq!(request["query"], "client_version=1.0.0");
}

#[tokio::test]
async fn codex_refresh_writes_back_to_auth_json() {
    let (chatgpt, cg) = chatgpt().await;
    let dir = tempfile::tempdir().unwrap();
    let auth = dir.path().join("auth.json");
    write_codex_auth(&auth, &codex_access_token(1_000), "ref-1", "acc-1");
    let session = dir.path().join("codex-session.json");
    let gateway = codex_gateway_with(
        &format!("{}/codex", chatgpt.base),
        &format!("{}/oauth/token", chatgpt.base),
        &auth,
        &session,
        None,
    )
    .await;
    let http = reqwest::Client::new();

    // The token is expired: the request triggers a refresh and retry.
    let collected: Value = http
        .post(format!("{}/v1/responses", gateway.base))
        .json(&json!({"model": "gpt-5.5", "input": []}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(collected["id"], "resp_1");

    let requests = cg.requests.lock().await;
    let refresh = requests
        .iter()
        .find(|r| r["path"] == "/oauth/token")
        .expect("refresh happened");
    assert_eq!(refresh["body"]["grant_type"], "refresh_token");
    assert_eq!(refresh["body"]["client_id"], "app_EMoamEEZ73f0CkXaXp7hrann");
    // The auth.json now carries the rotated token and updated last_refresh.
    let stored: Value = serde_json::from_str(&std::fs::read_to_string(&auth).unwrap()).unwrap();
    assert!(
        stored["tokens"]["access_token"]
            .as_str()
            .unwrap()
            .contains("eyJleHAi")
    );
    assert_eq!(stored["tokens"]["refresh_token"], "ref-2");
    assert_eq!(stored["tokens"]["account_id"], "acc-1");
    assert_eq!(stored["auth_mode"], "chatgpt");
    assert!(stored["last_refresh"].as_str().unwrap().starts_with("2"));
    // The session file was not created: the CLI's store stays canonical.
    assert!(!session.exists());
}

#[tokio::test]
async fn codex_no_credentials_returns_401() {
    let (chatgpt, _) = chatgpt().await;
    let dir = tempfile::tempdir().unwrap();
    let session = dir.path().join("missing.json");
    let gateway = codex_gateway_with(
        &format!("{}/codex", chatgpt.base),
        &format!("{}/oauth/token", chatgpt.base),
        &dir.path().join("missing-auth.json"),
        &session,
        None,
    )
    .await;
    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", gateway.base))
        .json(&json!({"model": "gpt-5.5", "input": []}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let error: Value = response.json().await.unwrap();
    assert!(
        error["error"]
            .as_str()
            .unwrap()
            .contains("No Codex credentials")
    );
}

#[tokio::test]
async fn gateway_serves_both_adapters_and_reports_status() {
    let (api, _) = anthropic().await;
    let (chatgpt, _) = chatgpt().await;
    let dir = tempfile::tempdir().unwrap();
    let claude_session = dir.path().join("claude-session.json");
    write_session(&claude_session, "tok-1", "ref-1");
    let codex_auth = dir.path().join("auth.json");
    write_codex_auth(
        &codex_auth,
        &codex_access_token(4_102_444_800),
        "ref-1",
        "acc-1",
    );

    let claude_config = ClaudeConfig {
        oauth_token: None,
        use_keychain: false,
        credentials_file: claude_session.clone(),
        session_file: claude_session.clone(),
        api_base: api.base.clone(),
        token_url: "http://unused/token".into(),
    };
    let claude_auth = Arc::new(claude_codex_api::claude::auth::TokenManager::new(
        claude_config.clone(),
    ));
    let claude_proxy = Arc::new(
        claude_codex_api::claude::proxy::ClaudeProxy::new(claude_config, claude_auth.clone())
            .unwrap(),
    );
    let codex_config = CodexConfig {
        oauth_token: None,
        auth_file: codex_auth.clone(),
        session_file: dir.path().join("codex-session.json"),
        api_base: format!("{}/codex", chatgpt.base),
        token_url: format!("{}/oauth/token", chatgpt.base),
    };
    let codex_auth_mgr = Arc::new(claude_codex_api::codex::auth::TokenManager::new(
        codex_config.clone(),
    ));
    let codex_proxy = Arc::new(
        claude_codex_api::codex::proxy::CodexProxy::new(codex_config, codex_auth_mgr.clone())
            .unwrap(),
    );
    let gateway = full_gateway(
        Some(claude_proxy),
        Some(claude_auth),
        Some(codex_proxy),
        Some(codex_auth_mgr),
        None,
    )
    .await;
    let http = reqwest::Client::new();

    let health: Value = http
        .get(format!("{}/healthz", gateway.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(health["adapters"], json!(["claude", "codex"]));

    let status: Value = http
        .get(format!("{}/auth/status", gateway.base))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["claude"]["authenticated"], true);
    assert_eq!(status["codex"]["authenticated"], true);
    assert_eq!(status["codex"]["account_id"], "acc-1");

    // Both proxies answer.
    let claude_response: Value = http
        .post(format!("{}/v1/messages", gateway.base))
        .json(&json!({"model": "claude-haiku-4-5", "max_tokens": 8, "messages": [{"role": "user", "content": "hi"}]}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(claude_response["content"][0]["text"], "Hello");
    let codex_response: Value = http
        .post(format!("{}/v1/responses", gateway.base))
        .json(&json!({"model": "gpt-5.5", "input": []}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(codex_response["id"], "resp_1");
}
