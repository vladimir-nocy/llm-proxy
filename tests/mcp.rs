mod common;

use std::{process::Stdio, time::Duration};

use common::*;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines},
    process::{Child, ChildStdin, ChildStdout, Command},
    time::timeout,
};

struct McpClient {
    child: Child,
    input: ChildStdin,
    output: Lines<BufReader<ChildStdout>>,
}

impl McpClient {
    async fn start(binary: &str, directory: &std::path::Path, vars: &[(&str, &str)]) -> Self {
        let mut child = Command::new(binary)
            .current_dir(directory)
            .env_clear()
            .envs(vars.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let output = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut client = Self {
            child,
            input,
            output,
        };
        let initialized = client.request(1, "initialize", json!({"protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "integration-test", "version": "1.0"}})).await;
        assert_eq!(
            initialized["result"]["serverInfo"]["name"], "claude-codex-api",
            "{initialized}"
        );
        assert!(initialized["result"]["capabilities"]["tools"].is_object());
        client
            .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        client
    }

    async fn send(&mut self, value: Value) {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .unwrap();
        self.input.flush().await.unwrap();
    }

    async fn request(&mut self, id: u64, method: &str, params: Value) -> Value {
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        timeout(Duration::from_secs(10), async {
            loop {
                let line = self
                    .output
                    .next_line()
                    .await
                    .unwrap()
                    .expect("MCP server closed stdout");
                let response: Value =
                    serde_json::from_str(&line).expect("stdout must only contain MCP JSON");
                if response["id"] == id {
                    return response;
                }
            }
        })
        .await
        .expect("MCP response timed out")
    }

    async fn close(mut self) {
        drop(self.input);
        assert!(
            timeout(Duration::from_secs(5), self.child.wait())
                .await
                .unwrap()
                .unwrap()
                .success()
        );
    }
}

#[tokio::test]
async fn rest_stdio_protocol_discovery_calls_errors_and_dotenv() {
    let (upstream, _) = upstream().await;
    let dir = tempfile::tempdir().unwrap();
    // Prove .env loads and an exported variable overrides the file.
    std::fs::write(
        dir.path().join(".env"),
        format!(
            "REST_BASE_URL={}\nREST_TOKEN=from-file\nREST_HEADERS='{{\"x-team\":\"studio\"}}'\n",
            upstream.base
        ),
    )
    .unwrap();
    let mut mcp = McpClient::start(
        env!("CARGO_BIN_EXE_claude-codex-api-rest-mcp"),
        dir.path(),
        &[("REST_TOKEN", "exported")],
    )
    .await;
    let tools = mcp.request(2, "tools/list", json!({})).await;
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 3);
    let (gateway, _) = gateway(rest_config(&upstream.base), None).await;
    let http_tools: Value = reqwest::get(format!("{}/tools", gateway.base))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tools["result"]["tools"], http_tools["tools"]);
    let response = mcp
        .request(
            3,
            "tools/call",
            json!({"name": "rest_get", "arguments": {"path": "/echo"}}),
        )
        .await;
    assert_ne!(response["result"]["isError"], true, "{response}");
    let result: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(result["headers"]["authorization"], "Bearer exported");
    assert_eq!(result["headers"]["x-team"], "studio");
    for (id, args) in [
        (
            4,
            json!({"name": "rest_get", "arguments": {"path": "/missing"}}),
        ),
        (5, json!({"name": "rest_get", "arguments": {}})),
        (6, json!({"name": "unknown", "arguments": {}})),
    ] {
        let response = mcp.request(id, "tools/call", args).await;
        assert_eq!(response["result"]["isError"], true, "{response}");
        assert!(response["result"]["content"][0]["text"].is_string());
    }
    let response = mcp
        .request(
            7,
            "tools/call",
            json!({"name": "rest_get", "arguments": {"path": "/empty"}}),
        )
        .await;
    assert_eq!(response["result"]["content"][0]["text"], "null");
    assert!(mcp.request(8, "ping", json!({})).await["result"].is_object());
    mcp.close().await;
}

#[tokio::test]
async fn http_executables_start_and_config_errors_fail_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    for (binary, extra) in [
        (
            env!("CARGO_BIN_EXE_claude-codex-api-rest-http"),
            vec![("REST_BASE_URL", "http://127.0.0.1:1")],
        ),
        (
            env!("CARGO_BIN_EXE_claude-codex-api-http"),
            vec![("CLAUDE_SESSION_FILE", "session.json")],
        ),
    ] {
        let mut child = Command::new(binary)
            .current_dir(dir.path())
            .env_clear()
            .envs(extra)
            .env("GATEWAY_PORT", "0")
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(child.stderr.take().unwrap()).lines();
        let banner = timeout(Duration::from_secs(10), lines.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let base = banner
            .split("listening on ")
            .nth(1)
            .expect("listening banner");
        let response: Value = reqwest::get(format!("{base}/healthz"))
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(response["ok"], true);
        child.kill().await.unwrap();
        child.wait().await.unwrap();
    }
    for vars in [
        vec![],
        vec![("REST_BASE_URL", "bad-url")],
        vec![
            ("REST_BASE_URL", "http://localhost"),
            ("REST_HEADERS", "[]"),
        ],
        vec![
            ("REST_BASE_URL", "http://localhost"),
            ("GATEWAY_PORT", "invalid"),
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_claude-codex-api-rest-http"))
            .current_dir(dir.path())
            .env_clear()
            .envs(vars)
            .output()
            .await
            .unwrap();
        assert!(!output.status.success());
        assert!(!output.stderr.is_empty());
    }
}
