#!/usr/bin/env node
/**
 * MCP entry point — what Claude Code and Codex CLI connect to.
 *
 *   Claude Code:  claude mcp add forge -- node .../dist/forge/mcp.js
 *   Codex CLI:    [mcp_servers.forge]  command = "node"  args = [".../dist/forge/mcp.js"]
 */
import { runMcpServer } from "../core/mcp.js";
import { ForgeClient } from "./client.js";
import { forgeConfigFromEnv } from "./config.js";
import { buildForgeTools } from "./tools.js";

const cfg = forgeConfigFromEnv();
await runMcpServer({
  name: "claude-codex-api",
  version: "0.1.0",
  tools: buildForgeTools(new ForgeClient(cfg)),
  banner: `claude-codex-api [forge adapter] MCP ready (forge: ${cfg.url})`,
});
