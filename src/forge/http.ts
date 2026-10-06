#!/usr/bin/env node
/**
 * HTTP entry point — the same tools as the MCP server, over plain HTTP.
 *
 *   GET  /healthz           liveness + config summary
 *   GET  /tools             tool list with JSON schemas
 *   POST /rpc               { "name": "...", "args": {...} }  -> tool result
 *   ALL  /forge/<path>      direct authenticated passthrough to Forge /api/v1
 */
import { startHttpGateway } from "../core/http.js";
import { gatewayConfig } from "../core/env.js";
import { ForgeClient } from "./client.js";
import { forgeConfigFromEnv } from "./config.js";
import { buildForgeTools } from "./tools.js";

const cfg = forgeConfigFromEnv();
const gw = gatewayConfig();

startHttpGateway({
  name: "forge-gateway",
  version: "0.1.0",
  tools: buildForgeTools(new ForgeClient(cfg)),
  client: new ForgeClient(cfg),
  passthrough: { prefix: "/forge" },
  bind: gw.bind,
  port: gw.port,
  token: gw.token,
  health: {
    forge: cfg.url,
    auth: cfg.cookie ? "cookie" : cfg.user ? "password" : "none",
  },
});
