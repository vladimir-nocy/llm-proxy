import { homedir } from "node:os";
import { resolve } from "node:path";

export function env(name: string): string | undefined {
  const v = process.env[name];
  return v && v.trim() !== "" ? v.trim() : undefined;
}

/** Settings shared by every gateway front-end (HTTP server, MCP, ...). */
export function gatewayConfig() {
  return {
    bind: env("GATEWAY_BIND") ?? "127.0.0.1",
    port: Number(env("GATEWAY_PORT") ?? 7781),
    /** Bearer token required by the HTTP API (recommended beyond loopback). */
    token: env("GATEWAY_TOKEN"),
  };
}

/** Default on-disk session cache path for a named adapter. */
export function defaultSessionFile(adapter: string): string {
  return resolve(homedir(), ".claude-codex-api", `${adapter}-session.json`);
}
