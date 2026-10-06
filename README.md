# forge-gateway

**Let Claude Code and Codex CLI call your studio's API as native agent tools.**

`forge-gateway` is a small sidecar service that sits between CLI coding agents
and a studio server (**Forge**, `http://sumos-macbook-pro.local:7700` by
default). It owns the authenticated session and exposes the API as tools over
two protocols at once:

```
Claude Code ─┐                              ┌── MCP (stdio)   ← agents use it as native tools
             ├─►  forge-gateway  ──►  Forge │
Codex CLI  ──┘    (this repo)               └── HTTP (curl)   ← scripts, workflows, you
                   · owns the login session
                   · normalizes errors
                   · curated tools + generic passthrough
```

- **MCP over stdio** — both CLIs support MCP servers natively; the tools show
  up as regular agent tools (`forge_list_entities`, `forge_add_comment`, …).
- **HTTP + JSON** — the same tools via `POST /rpc`, plus a direct authenticated
  passthrough at `/forge/<path>` for anything the curated tools don't cover.

The gateway logs in with username/password (or a session cookie you paste from
your browser), caches the session to disk so restarts and both processes share
one login, attaches Forge's `X-Forge-Csrf` header on mutations, and maps
problem+json errors into clean agent-readable messages.

## Install

```sh
git clone https://github.com/<you>/forge-gateway
cd forge-gateway
npm install && npm run build
cp .env.example .env   # then fill in auth (see below)
```

Requires Node 20+.

## Forge auth (pick one)

**A. Username + password** — set in `.env`:

```
FORGE_USER=your-username
FORGE_PASSWORD=your-password
```

**B. Pasted cookie** (no password needed): sign in to Forge in your browser,
DevTools → Application → Cookies → copy the session cookie value:

```
FORGE_COOKIE=forge_session=...
```

With credentials configured, an expired session is automatically re-logged-in
on the next call. With a pasted cookie, replace it when it expires.

## Run

```sh
npm start            # builds + starts the HTTP API on http://127.0.0.1:7781
# or individually:
node dist/forge/http.js    # HTTP API
node dist/forge/mcp.js     # MCP stdio server (started by the CLIs, not by you)
```

## Wire up the agents

**Claude Code:**

```sh
claude mcp add forge -- node /path/to/forge-gateway/dist/forge/mcp.js
```

**Codex CLI** — add to `~/.codex/config.toml`:

```toml
[mcp_servers.forge]
command = "node"
args = ["/path/to/forge-gateway/dist/forge/mcp.js"]
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
  (e.g. so Forge itself can call it), set `GATEWAY_BIND=0.0.0.0` **and**
  `GATEWAY_TOKEN=<long-random-string>`; callers then need
  `Authorization: Bearer <token>`.
- Without a token, loopback-only is enforced.
- Agents cannot touch `/auth/login`, `/auth/logout` or `/auth/invites` —
  the gateway blocks those routes so an agent can't lock you out.

## Tools

Curated (verified against Forge's own frontend routes):

`forge_status`, `forge_list_entities`, `forge_get_entity`, `forge_create_entity`,
`forge_list_runs`, `forge_get_run`, `forge_cancel_run`, `forge_list_snapshots`,
`forge_create_snapshot`, `forge_add_comment`, `forge_list_adapters`,
`forge_adapter_action`

Plus the escape hatch: `forge_request` — any `GET/POST/PATCH/PUT/DELETE`
against `/api/v1/*` (entities, runs, budgets, comments, snapshots,
`/battle/*`, …).

## Adding another API

The core (`src/core/`) is upstream-agnostic: a cookie-session API client, an
MCP runner and an HTTP gateway. An "adapter" is just a config + a client
subclass + a tool list — see `src/forge/` (~200 lines total). To wire a
different studio/API:

1. `src/<name>/config.ts` — env vars for its URL and auth.
2. `src/<name>/client.ts` — extend `ApiClient` (login endpoint, extra headers,
   blocked routes).
3. `src/<name>/tools.ts` — curate `ToolDef`s for its routes.
4. `src/<name>/mcp.ts` + `http.ts` — two small entry points; add `bin` entries
   in `package.json`.

## Development

```sh
npm test            # build + offline smoke test (spins up the HTTP gateway)
```

## Phase 2 (not built yet): studio → agents

The reverse direction — the studio spawning agent work — is the same pattern
inverted: a small runner that the gateway (or Forge adapters) calls, which
spawns `claude -p --output-format stream-json` or `codex exec --json` as
subprocesses and streams events back. The gateway's HTTP API is already the
natural place for those `/agents/*` endpoints to live.

## License

[MIT](LICENSE)
