# claude-codex-api

**Expose any web API as native agent tools for Claude Code and Codex CLI.**

`claude-codex-api` is a small sidecar that sits between CLI coding agents and a
web API. It owns the authenticated session and exposes the API as tools over
two protocols at once:

```
Claude Code ─┐                                   ┌── MCP (stdio)   ← agents use it as native tools
             ├─►  claude-codex-api  ──►  any API │
Codex CLI  ──┘    (this repo)                    └── HTTP (curl)   ← scripts, workflows, you
                       · owns the login session
                    · normalizes errors
                    · curated tools + generic passthrough
```

- **MCP over stdio** — both CLIs support MCP servers natively; the tools show
  up as regular agent tools.
- **HTTP + JSON** — the same tools via `POST /rpc`, plus a direct authenticated
  passthrough at `/<adapter>/<path>` for anything the curated tools don't cover.

It is **not tied to any one product**: the core (`src/core/`) is upstream-
agnostic — a cookie-session API client, an MCP runner and an HTTP gateway.
Specific services are wired in as **adapters**; this repo ships one
(**Forge**, a game-studio pipeline API) as a working example, and adding your
own is ~200 lines (see [Adding another API](#adding-another-api)).

## Install

```sh
git clone https://github.com/billy-boys/claude-codex-api
cd claude-codex-api
npm install && npm run build
cp .env.example .env   # then fill in the adapter's auth (see below)
```

Requires Node 20+.

## Bundled adapter: Forge

The Forge adapter talks to a Forge studio server (`FORGE_URL`). It logs in
with username/password — or a session cookie you paste from your browser —
caches the session to disk so restarts and both processes share one login,
attaches Forge's `X-Forge-Csrf` header on mutations, and maps problem+json
errors into clean agent-readable messages.

**Auth (pick one)** — in `.env`:

```
# A. username + password (auto re-login on expiry)
FORGE_USER=your-username
FORGE_PASSWORD=your-password

# B. pasted cookie (no password needed):
# browser → DevTools → Application → Cookies → copy the session cookie
FORGE_COOKIE=forge_session=...
```

**Run:**

```sh
npm start                  # HTTP API on http://127.0.0.1:7781
node dist/forge/mcp.js     # MCP stdio server (started by the CLIs, not by you)
```

**Wire up the agents:**

```sh
# Claude Code
claude mcp add forge -- node /path/to/claude-codex-api/dist/forge/mcp.js
```

```toml
# Codex CLI — ~/.codex/config.toml
[mcp_servers.forge]
command = "node"
args = ["/path/to/claude-codex-api/dist/forge/mcp.js"]
```

Then ask either agent things like:

- "What adapters does Forge have and are any unhealthy?"
- "List the draft asset entities and summarize what's missing."
- "Add a comment on entity `hero_body` saying the silhouette needs work."
- "What's the status of run `<id>`? Cancel it if it's stuck."

## HTTP API

| Route | Description |
| --- | --- |
| `GET /healthz` | liveness + config summary |
| `GET /tools` | every tool with its JSON schema |
| `POST /rpc` | `{"name":"forge_list_entities","args":{...}}` → `{"ok":true,"result":...}` |
| `ANY /forge/<path>` | authenticated passthrough to `FORGE_URL/api/v1/<path>` |

Examples:

```sh
curl -s localhost:7781/rpc -H 'content-type: application/json' \
  -d '{"name":"forge_list_adapters"}'

curl -s localhost:7781/forge/entities -X POST -H 'content-type: application/json' \
  -d '{"type":"asset","id":"my_thing","body":{},"message":"created by agent"}'
```

### Security

- The HTTP server binds `127.0.0.1` by default. To expose it on the LAN
  (e.g. so other services can call it), set `GATEWAY_BIND=0.0.0.0` **and**
  `GATEWAY_TOKEN=<long-random-string>`; callers then need
  `Authorization: Bearer <token>`.
- Without a token, loopback-only is enforced.
- Auth-dangerous upstream routes can be blocked per adapter — the Forge
  adapter blocks `/auth/login`, `/auth/logout` and `/auth/invites` so an
  agent can't lock you out.

## Tools

Each adapter curates tool definitions (`ToolDef`) that are exposed identically
over MCP and HTTP. The Forge adapter ships:

`forge_status`, `forge_list_entities`, `forge_get_entity`, `forge_create_entity`,
`forge_list_runs`, `forge_get_run`, `forge_cancel_run`, `forge_list_snapshots`,
`forge_create_snapshot`, `forge_add_comment`, `forge_list_adapters`,
`forge_adapter_action`

Plus the escape hatch: `forge_request` — any `GET/POST/PATCH/PUT/DELETE`
against `/api/v1/*` (entities, runs, budgets, comments, snapshots,
`/battle/*`, …).

## Adding another API

An adapter is just a config + a client subclass + a tool list — see
`src/forge/` (~200 lines total). To wire a different API:

1. `src/<name>/config.ts` — env vars for its URL and auth.
2. `src/<name>/client.ts` — extend `ApiClient` (login endpoint, extra headers,
   blocked routes). Any cookie-session JSON API works out of the box.
3. `src/<name>/tools.ts` — curate `ToolDef`s for its routes (tool names are
   conventionally prefixed with the adapter name, e.g. `forge_*`).
4. `src/<name>/mcp.ts` + `http.ts` — two small entry points; add `bin` entries
   in `package.json`.

The core needs nothing changed: `ApiClient` handles session caching, login
retries and header injection; `runMcpServer` and `startHttpGateway` take any
tool list.

## Development

```sh
npm test            # build + offline smoke test (spins up the HTTP gateway)
```

## Roadmap: the reverse direction

The mirror image — agents *run by* the gateway — is the same pattern inverted:
a runner that spawns `claude -p --output-format stream-json` (Claude Code's
headless protocol) or `codex app-server` (Codex's JSON-RPC harness) as
subprocesses and streams normalized events back over HTTP. The gateway's HTTP
API is the natural home for those `/agents/*` endpoints.

## License

[MIT](LICENSE)
