#!/usr/bin/env node
/**
 * HTTP entry point for the generic REST adapter.
 *
 *   GET  /healthz         liveness + target API summary
 *   GET  /tools           tool list with JSON schemas
 *   POST /rpc             { "name": "rest_get", "args": {...} }
 *   ANY  /rest/<path>     authenticated passthrough to REST_BASE_URL/<path>
 */
import { startHttpGateway } from "../core/http.js";
import { gatewayConfig } from "../core/env.js";
import { RestClient } from "./client.js";
import { restConfigFromEnv } from "./config.js";
import { buildRestTools } from "./tools.js";

const cfg = restConfigFromEnv();
if (!cfg.baseUrl) {
  console.error("REST_BASE_URL is not set — point it at the API you want to expose, e.g. https://api.github.com");
  process.exit(1);
}
const gw = gatewayConfig();

startHttpGateway({
  name: "claude-codex-api",
  version: "0.1.0",
  tools: buildRestTools(new RestClient(cfg)),
  client: new RestClient(cfg),
  passthrough: { prefix: "/rest" },
  bind: gw.bind,
  port: gw.port,
  token: gw.token,
  health: {
    base: cfg.baseUrl,
    auth: cfg.token ? "bearer" : cfg.apiKey ? "api-key" : cfg.headers ? "custom-headers" : "none",
  },
});
