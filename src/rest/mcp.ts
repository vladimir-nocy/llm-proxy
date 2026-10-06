#!/usr/bin/env node
/**
 * MCP entry point for the generic REST adapter — point it at any API.
 *
 *   claude mcp add myapi -- node .../dist/rest/mcp.js
 *   codex mcp add myapi  -- node .../dist/rest/mcp.js
 */
import { runMcpServer } from "../core/mcp.js";
import { RestClient } from "./client.js";
import { restConfigFromEnv } from "./config.js";
import { buildRestTools } from "./tools.js";

const cfg = restConfigFromEnv();
if (!cfg.baseUrl) {
  console.error("REST_BASE_URL is not set — point it at the API you want to expose, e.g. https://api.github.com");
  process.exit(1);
}

await runMcpServer({
  name: "claude-codex-api",
  version: "0.1.0",
  tools: buildRestTools(new RestClient(cfg)),
  banner: `claude-codex-api [rest adapter] MCP ready (base: ${cfg.baseUrl})`,
});
