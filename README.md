# llm-proxy

Two things in one Rust binary set, no Node.js runtime required:

- **Claude adapter**: expose a Claude Pro/Max/Team subscription as an
  Anthropic-compatible HTTP API. The gateway signs requests as a Claude Code
  CLI session (OAuth bearer token, fingerprint headers, billing cloak) and
  forwards them to `api.anthropic.com`, so any Anthropic SDK or OpenAI-style
  client can spend your subscription instead of API credits.
- **Codex adapter**: expose a ChatGPT Plus/Pro subscription as a Responses
  API. The gateway signs requests as the Codex CLI (bearer token,
  `chatgpt-account-id`, `originator: codex_cli_rs`) and forwards them to
  `chatgpt.com/backend-api/codex`, so any OpenAI SDK can spend your ChatGPT
  plan instead of API credits.
- **REST adapter**: expose any JSON web API as tools for coding agents
  (MCP) and as authenticated HTTP endpoints for scripts.

```text
Anthropic SDK ── POST /v1/messages ──┐
curl ────────── GET /usage ──────────┤ Claude adapter ── OAuth ── api.anthropic.com
browser ─────── POST /auth/login ────┘

OpenAI SDK ──── POST /v1/responses ──┐
curl ────────── GET /v1/models ──────┤ Codex adapter ── OAuth ── chatgpt.com/backend-api/codex
curl ────────── GET /usage/codex ────┘

Coding agent ── MCP over stdio ──┐
                               ├── Tool registry ── Authenticated client ── JSON API
Scripts/curl ── HTTP /rpc ───────┘   (REST adapter)
```

## Build and install

Requires Rust 1.88+ and a native C/C++ build toolchain for TLS dependencies. On macOS, install Xcode Command Line Tools; on Windows, use the MSVC Rust toolchain with Visual Studio C++ Build Tools and CMake.

```sh
git clone https://github.com/vladimir-nocy/llm-proxy
cd llm-proxy
cargo build --release --locked
cp .env.example .env
```

Edit `.env` to configure the adapter. Executables automatically load `.env` from the working directory or its nearest parent containing one. Exported environment variables take precedence.

The binaries are in `target/release/` (with `.exe` on Windows):

| Binary | Adapters | Interface |
| --- | --- | --- |
| `llm-proxy-http` | Claude + Codex | HTTP |
| `llm-proxy-rest-http` | REST | HTTP |
| `llm-proxy-rest-mcp` | REST | MCP stdio |

To install them into Cargo's binary directory:

```sh
cargo install --path . --locked
```

## Claude adapter

The gateway presents itself to Anthropic as a Claude Code CLI session:

- `Authorization: Bearer <oauth access token>` (never `x-api-key`)
- `anthropic-beta: claude-code-20250219,oauth-2025-04-20,…`
- Claude Code fingerprint headers (`x-app: cli`, `user-agent: claude-cli/…`, `x-stainless-*`, `x-claude-code-session-id`)
- The billing cloak: a `x-anthropic-billing-header: cc_version=…; cc_entrypoint=cli;` system block and the "You are Claude Code…" prefix, plus `metadata.user_id` — without these, OAuth tokens cannot reach Sonnet and Opus

### Credentials

Token sources, in precedence order:

1. `CLAUDE_CODE_OAUTH_TOKEN` (or `CLAUDE_OAUTH_TOKEN`) — a literal token; never refreshed
2. The gateway's session file (`CLAUDE_SESSION_FILE`, default `~/.llm-proxy/claude-session.json`) — tokens minted by the login flow and all refresh results
3. Claude Code's credentials (`~/.claude/.credentials.json`, or the macOS Keychain entry `Claude Code-credentials` unless `CLAUDE_USE_KEYCHAIN=0`)

Expired tokens refresh automatically against `platform.claude.com/v1/oauth/token` (single-flight, exponential backoff, refresh results persisted to the session file). If a refresh token is revoked, the gateway falls through to the next source.

### Login flow

Without any stored credential, start a login:

```sh
curl -s -X POST http://127.0.0.1:7781/auth/login
# → {"status":"pending","started_at":"…"} and the browser opens
```

The gateway runs the OAuth PKCE flow (callback on `localhost:54545`), opens your browser at `claude.ai/oauth/authorize`, and stores the token on completion. Check progress with `GET /auth/status`.

```dotenv
# Optional overrides
# CLAUDE_API_BASE=https://api.anthropic.com
# CLAUDE_TOKEN_URL=https://platform.claude.com/v1/oauth/token
# CLAUDE_SESSION_FILE=~/.llm-proxy/claude-session.json
# CLAUDE_CREDENTIALS_FILE=~/.claude/.credentials.json
# CLAUDE_USE_KEYCHAIN=1

# HTTP API auth (required for callers beyond loopback)
# GATEWAY_TOKEN=some-long-random-string
# GATEWAY_PORT=7781
# GATEWAY_BIND=127.0.0.1
```

### HTTP API

| Route | Behavior |
| --- | --- |
| `POST /v1/messages` | Anthropic Messages passthrough (JSON or SSE stream), signed as Claude Code |
| `POST /v1/messages/count_tokens` | Count-tokens passthrough |
| `GET /usage` | Claude plan usage via a minimal probe; the unified rate-limit snapshot |
| `POST /v1/responses` | Codex Responses passthrough (SSE stream, or the collected final response for non-stream clients) |
| `GET /v1/models` | The ChatGPT plan's model catalog |
| `GET /usage/codex` | Last captured Codex rate-limit snapshot (`x-codex-*` headers) |
| `POST /auth/login` | Start the Claude OAuth login flow (opens the browser) |
| `GET /auth/status` | Credential state for both adapters |
| `POST /auth/logout` | Forget cached tokens and delete both session files |
| `GET /healthz` | Liveness and configuration summary |

```sh
curl -s http://127.0.0.1:7781/v1/messages \
  -H 'content-type: application/json' \
  -d '{"model":"claude-sonnet-4-5","max_tokens":1024,"messages":[{"role":"user","content":"Hello"}]}'

curl -s http://127.0.0.1:7781/v1/responses \
  -H 'content-type: application/json' \
  -d '{"model":"gpt-5.5","input":[{"type":"message","role":"user","content":[{"type":"input_text","text":"Hello"}]}]}'
```

Responses pass through verbatim, including upstream error statuses and SSE streams. A 401 triggers one token refresh and retry. Request bodies are limited to 2 MiB; upstream calls time out after 600 seconds. Without `GATEWAY_TOKEN`, only loopback callers are accepted; with one, every route requires `Authorization: Bearer <GATEWAY_TOKEN>`.

> **Note**: this drives your subscriptions' rate limits (`anthropic-ratelimit-unified-*`, `x-codex-*`), not API rate limits, and automated non-CLI usage of subscription tokens sits in a gray zone of Anthropic's and OpenAI's terms. Peer projects ([BYOKEY](https://github.com/AprilNEA/BYOKEY), [ocp](https://github.com/dtzp555-max/ocp), [llm-bridge](https://github.com/samirsawarkar/llm-bridge)) take the same approach.

### Codex credentials

Token sources, in precedence order:

1. `CODEX_OAUTH_TOKEN` — a literal token; never refreshed
2. The gateway's session file (`CODEX_SESSION_FILE`, default `~/.llm-proxy/codex-session.json`)
3. Codex CLI's credentials (`$CODEX_HOME/auth.json`, default `~/.codex/auth.json`)

There is no browser login flow: run `codex login` once on the machine, the gateway picks up the credential from there. The access token's expiry comes from its JWT `exp` claim; expired tokens refresh against `auth.openai.com/oauth/token` (single-flight, exponential backoff), and because OpenAI refresh tokens rotate, the result is written back to `auth.json` so the CLI stays logged in. If a refresh token is revoked, the gateway falls through to the next source.

```dotenv
# Optional overrides
# CODEX_API_BASE=https://chatgpt.com/backend-api/codex
# CODEX_TOKEN_URL=https://auth.openai.com/oauth/token
# CODEX_SESSION_FILE=~/.llm-proxy/codex-session.json
# CODEX_AUTH_FILE=~/.codex/auth.json
```

## REST adapter

```dotenv
REST_BASE_URL=https://api.github.com
# REST_BASE_PATH=/api/v1
# REST_TOKEN=your-token
# REST_API_KEY=your-key
# REST_API_KEY_HEADER=x-api-key
# REST_HEADERS='{"x-team":"studio"}'
```

`REST_BASE_URL` is required. `REST_BASE_PATH` is optional and must start with `/`. Authentication is optional; bearer tokens, API keys, and custom headers can be combined. Explicit token and API-key settings override matching custom header names.

Tools:

- `rest_get`: `path`, optional `query`.
- `rest_post`: `path`, optional `query` and JSON `body`.
- `rest_request`: `method`, `path`, optional `query` and JSON `body`. Methods: GET, POST, PATCH, PUT, DELETE.

Paths begin with `/` and are appended to the configured API base. Query values are strings. An explicit query argument overrides the same parameter embedded in a path. JSON bodies support objects, arrays, strings, numbers, booleans, and null.

```sh
curl -s http://127.0.0.1:7781/rpc \
  -H 'content-type: application/json' \
  -d '{"name":"rest_get","args":{"path":"/repos/facebook/react"}}'
```

Configure an MCP client to launch the absolute path to `llm-proxy-rest-mcp`, with no command arguments and the REST environment settings above. The client communicates over stdin/stdout; diagnostics go to stderr.

## HTTP API (REST adapter)

Default address: `127.0.0.1:7781`. Override it using `GATEWAY_BIND` and `GATEWAY_PORT`.

| Route | Behavior |
| --- | --- |
| `GET /healthz` | Liveness and configuration summary; does not contact the upstream API |
| `GET /tools` | Tool names, descriptions, and JSON input schemas |
| `POST /rpc` | `{ "name": "tool_name", "args": {...} }` → `{ "ok": true, "result": ... }` |
| `/rest/<path>` | Direct authenticated API request |

The gateway uses configured upstream credentials, not the caller's headers. Without `GATEWAY_TOKEN`, only loopback callers are accepted. If a token is set, every HTTP route requires `Authorization: Bearer <GATEWAY_TOKEN>`, including requests from localhost. For LAN access set both `GATEWAY_BIND=0.0.0.0` and a token.

Both transports validate tool arguments against the same JSON schemas. HTTP errors use `{ "error": "..." }`, preserving upstream error status and problem responses' `current` field. MCP failures use tool results with `isError: true` and readable text. The REST gateway buffers responses, returns successful calls as 200, and does not forward upstream response headers. An empty response becomes `null` in tool results and `{ "ok": true }` in passthrough responses. Redirects are returned as errors rather than followed, so configured credentials are not sent to a redirected destination.

## Development

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
```

Tests use local mock APIs and ephemeral ports, with temporary session files. They cover tool mappings, auth headers, HTTP passthrough/errors, OAuth refresh and fallback, the billing cloak, streaming passthrough, executable startup, `.env` loading, and MCP stdio initialization/discovery/calls. They do not contact Anthropic or another external API. CI is configured for Linux, macOS, and Windows, with an additional Rust 1.88 compatibility check.

On a Mac with mismatched Command Line Tools and default SDK versions, select an SDK supported by the installed linker, for example:

```sh
SDKROOT=/Library/Developer/CommandLineTools/SDKs/MacOSX26.5.sdk cargo test --all-targets --locked
```

Code layout:

- `src/claude/auth.rs`: OAuth token chain (env → session file → Claude Code credentials → Keychain), PKCE login flow, refresh with backoff and write-back.
- `src/claude/cloak.rs`: billing-header fingerprint and system-prompt cloak.
- `src/claude/proxy.rs`: signed upstream requests, 401-refresh retry, JSON/SSE passthrough.
- `src/codex/auth.rs`: Codex token chain (env → session file → `auth.json`), JWT expiry, JSON refresh grant with write-back.
- `src/codex/proxy.rs`: Responses passthrough (SSE collect for non-stream clients), model catalog, rate-limit capture.
- `src/gateway_http.rs`: the subscription gateway's HTTP routes (both adapters).
- `src/client.rs`, `config.rs`, `error.rs`: REST upstream requests, environment configuration, and errors.
- `src/tools.rs`, `src/rest.rs`: tool definitions, validation, and execution.
- `src/http.rs`, `src/mcp.rs`: REST HTTP and MCP transports. MCP uses the [official Rust SDK](https://github.com/modelcontextprotocol/rust-sdk).
- `src/bin/`: executable entry points.

## License

[MIT](LICENSE)
