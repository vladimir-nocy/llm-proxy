use std::{collections::HashMap, path::PathBuf};

use crate::{
    claude::auth::OAUTH_TOKEN_URL,
    error::{GatewayError, Result},
};

pub fn env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_owned())
        .filter(|v| !v.is_empty())
}

/// Load .env from the working directory or its parents; exported values win.
pub fn load_dotenv() -> std::result::Result<(), dotenvy::Error> {
    match dotenvy::dotenv() {
        Ok(_) => Ok(()),
        Err(dotenvy::Error::Io(e)) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub bind: String,
    pub port: u16,
    pub token: Option<String>,
}

impl GatewayConfig {
    pub fn from_env() -> Result<Self> {
        Ok(Self {
            bind: env("GATEWAY_BIND").unwrap_or_else(|| "127.0.0.1".into()),
            port: env("GATEWAY_PORT")
                .unwrap_or_else(|| "7781".into())
                .parse()
                .map_err(|_| GatewayError::bad_request("GATEWAY_PORT must be a valid TCP port"))?,
            token: env("GATEWAY_TOKEN"),
        })
    }
}

#[derive(Clone)]
pub struct RestConfig {
    pub base_url: String,
    pub base_path: String,
    pub token: Option<String>,
    pub api_key: Option<String>,
    pub api_key_header: String,
    pub headers: HashMap<String, String>,
}

#[derive(Clone)]
pub struct ClaudeConfig {
    /// Explicit OAuth token (highest precedence, never refreshed).
    pub oauth_token: Option<String>,
    /// Claude Code's own credentials file (~/.claude/.credentials.json).
    pub credentials_file: PathBuf,
    /// Where tokens minted by the gateway's own login flow are stored.
    pub session_file: PathBuf,
    /// Anthropic API base URL.
    pub api_base: String,
    /// OAuth token endpoint.
    pub token_url: String,
    /// Read Claude Code's macOS Keychain entry as a credential source.
    pub use_keychain: bool,
}

impl ClaudeConfig {
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| GatewayError::bad_request("Home directory unavailable"))?;
        let credentials_file = env("CLAUDE_CREDENTIALS_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude/.credentials.json"));
        let session_file = env("CLAUDE_SESSION_FILE")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude-codex-api/claude-session.json"));
        Ok(Self {
            oauth_token: env("CLAUDE_CODE_OAUTH_TOKEN").or_else(|| env("CLAUDE_OAUTH_TOKEN")),
            credentials_file,
            session_file,
            api_base: env("CLAUDE_API_BASE").unwrap_or_else(|| "https://api.anthropic.com".into()),
            token_url: env("CLAUDE_TOKEN_URL").unwrap_or_else(|| OAUTH_TOKEN_URL.to_owned()),
            use_keychain: env("CLAUDE_USE_KEYCHAIN").map(|v| v != "0").unwrap_or(true),
        })
    }
}

#[derive(Clone)]
pub struct CodexConfig {
    /// Explicit OAuth access token (highest precedence, never refreshed).
    pub oauth_token: Option<String>,
    /// Codex CLI's credentials file (`$CODEX_HOME/auth.json`).
    pub auth_file: PathBuf,
    /// Where tokens minted or refreshed by the gateway are stored.
    pub session_file: PathBuf,
    /// ChatGPT Codex backend base URL.
    pub api_base: String,
    /// OAuth token endpoint.
    pub token_url: String,
}

impl CodexConfig {
    pub fn from_env() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| GatewayError::bad_request("Home directory unavailable"))?;
        let codex_home = env("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        Ok(Self {
            oauth_token: env("CODEX_OAUTH_TOKEN"),
            auth_file: env("CODEX_AUTH_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| codex_home.join("auth.json")),
            session_file: env("CODEX_SESSION_FILE")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".claude-codex-api/codex-session.json")),
            api_base: env("CODEX_API_BASE")
                .unwrap_or_else(|| "https://chatgpt.com/backend-api/codex".into()),
            token_url: env("CODEX_TOKEN_URL")
                .unwrap_or_else(|| "https://auth.openai.com/oauth/token".into()),
        })
    }
}

#[derive(Clone)]
pub enum AdapterConfig {
    Rest(RestConfig),
    Claude(ClaudeConfig),
    Codex(CodexConfig),
}

impl AdapterConfig {
    pub fn rest_from_env() -> Result<Self> {
        Ok(Self::Rest(RestConfig {
            base_url: env("REST_BASE_URL")
                .ok_or_else(|| GatewayError::bad_request("REST_BASE_URL is not set"))?,
            base_path: env("REST_BASE_PATH").unwrap_or_default(),
            token: env("REST_TOKEN"),
            api_key: env("REST_API_KEY"),
            api_key_header: env("REST_API_KEY_HEADER").unwrap_or_else(|| "x-api-key".into()),
            headers: env("REST_HEADERS")
                .map(|raw| serde_json::from_str(&raw))
                .transpose()
                .map_err(|_| {
                    GatewayError::bad_request(
                        "REST_HEADERS must be a JSON object with string values",
                    )
                })?
                .unwrap_or_default(),
        }))
    }

    pub fn claude_from_env() -> Result<Self> {
        Ok(Self::Claude(ClaudeConfig::from_env()?))
    }

    pub fn codex_from_env() -> Result<Self> {
        Ok(Self::Codex(CodexConfig::from_env()?))
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::Rest(_) => "rest",
            Self::Claude(_) => "claude",
            Self::Codex(_) => "codex",
        }
    }

    pub fn auth_mode(&self) -> &'static str {
        match self {
            Self::Rest(c) if c.token.is_some() => "bearer",
            Self::Rest(c) if c.api_key.is_some() => "api-key",
            Self::Rest(c) if !c.headers.is_empty() => "custom-headers",
            Self::Claude(_) => "oauth",
            Self::Codex(_) => "oauth",
            _ => "none",
        }
    }
}
