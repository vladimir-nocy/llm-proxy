pub mod claude;
pub mod client;
pub mod codex;
pub mod config;
pub mod error;
pub mod gateway_http;
pub mod http;
pub mod mcp;
pub mod rest;
pub mod tools;

use std::sync::Arc;

use client::ApiClient;
use config::{AdapterConfig, GatewayConfig};
use tools::ToolRegistry;

pub const NAME: &str = "claude-codex-api";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// Shared startup for the standalone executables: `"claude"` serves the
/// subscription gateway (Claude Messages + Codex Responses) over HTTP;
/// `"rest"` serves the generic API gateway over HTTP (`mcp = false`) or MCP
/// stdio (`mcp = true`).
pub async fn run(adapter: &str, mcp: bool) -> Result<(), Box<dyn std::error::Error>> {
    config::load_dotenv()?;
    match adapter {
        "claude" => {
            let claude_config = AdapterConfig::claude_from_env()?;
            let AdapterConfig::Claude(claude) = &claude_config else {
                unreachable!()
            };
            let claude_auth = Arc::new(claude::auth::TokenManager::new(claude.clone()));
            let claude_proxy = Arc::new(claude::proxy::ClaudeProxy::new(
                claude.clone(),
                claude_auth.clone(),
            )?);
            let codex_config = AdapterConfig::codex_from_env()?;
            let AdapterConfig::Codex(codex) = &codex_config else {
                unreachable!()
            };
            let codex_auth = Arc::new(codex::auth::TokenManager::new(codex.clone()));
            let codex_proxy = Arc::new(codex::proxy::CodexProxy::new(
                codex.clone(),
                codex_auth.clone(),
            )?);
            let gateway = GatewayConfig::from_env()?;
            let listener =
                tokio::net::TcpListener::bind((gateway.bind.as_str(), gateway.port)).await?;
            eprintln!(
                "{NAME} [claude+codex adapters] HTTP listening on http://{}",
                listener.local_addr()?
            );
            axum::serve(
                listener,
                gateway_http::router(
                    claude_proxy,
                    claude_auth,
                    codex_proxy,
                    codex_auth,
                    gateway.token,
                )
                .into_make_service_with_connect_info::<std::net::SocketAddr>(),
            )
            .with_graceful_shutdown(async {
                let _ = tokio::signal::ctrl_c().await;
            })
            .await?;
        }
        "rest" => {
            let config = AdapterConfig::rest_from_env()?;
            let client = Arc::new(ApiClient::new(config)?);
            let tools = Arc::new(ToolRegistry::new(client.clone())?);
            if mcp {
                mcp::serve(tools).await?;
            } else {
                let gateway = GatewayConfig::from_env()?;
                let listener =
                    tokio::net::TcpListener::bind((gateway.bind.as_str(), gateway.port)).await?;
                eprintln!(
                    "{NAME} [rest adapter] HTTP listening on http://{}",
                    listener.local_addr()?
                );
                axum::serve(
                    listener,
                    http::router(client, tools, gateway.token)
                        .into_make_service_with_connect_info::<std::net::SocketAddr>(),
                )
                .with_graceful_shutdown(async {
                    let _ = tokio::signal::ctrl_c().await;
                })
                .await?;
            }
        }
        other => {
            return Err(
                format!("Unknown adapter {other:?}: expected \"claude\" or \"rest\"").into(),
            );
        }
    }
    Ok(())
}
