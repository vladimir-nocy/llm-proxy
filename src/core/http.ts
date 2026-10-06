import { createServer, type IncomingMessage, type ServerResponse } from "node:http";
import { ApiClient } from "./client.js";
import { GatewayError, errorMessage } from "./errors.js";
import { shapeToJsonSchema } from "./jsonschema.js";
import type { ToolDef } from "./types.js";

export type HttpGatewayOptions = {
  name: string;
  version: string;
  tools: ToolDef[];
  /** Used by the passthrough route. */
  client: ApiClient;
  /** Proxy `PREFIX/<path>` to the upstream API (e.g. { prefix: "/forge" }). */
  passthrough?: { prefix: string };
  bind: string;
  port: number;
  /** Bearer token; when unset, loopback-only access is enforced. */
  token?: string;
  /** Extra fields for /healthz (upstream URL, auth mode, ...). */
  health?: Record<string, unknown>;
};

/**
 * Plain HTTP twin of the MCP server: the same tools via POST /rpc, plus an
 * authenticated passthrough to the upstream API. Useful for curl, scripts,
 * or wiring other systems (like Forge workflows) to the agents' toolkit.
 */
export function startHttpGateway(opts: HttpGatewayOptions): void {
  const tools = new Map(opts.tools.map((t) => [t.name, t]));
  const prefix = opts.passthrough?.prefix.replace(/\/+$/, "");

  function send(res: ServerResponse, status: number, body: unknown): void {
    res.writeHead(status, { "content-type": "application/json" });
    res.end(JSON.stringify(body, null, 2));
  }

  async function readJson(req: IncomingMessage): Promise<unknown> {
    const chunks: Buffer[] = [];
    for await (const chunk of req) chunks.push(chunk as Buffer);
    const text = Buffer.concat(chunks).toString("utf8");
    return text ? JSON.parse(text) : undefined;
  }

  function authorized(req: IncomingMessage, remote: string | undefined): boolean {
    if (!opts.token) {
      // No token configured: only allow loopback callers.
      return remote === "127.0.0.1" || remote === "::1" || remote === "::ffff:127.0.0.1";
    }
    return req.headers.authorization === `Bearer ${opts.token}`;
  }

  const server = createServer(async (req, res) => {
    const url = new URL(req.url ?? "/", `http://${req.headers.host ?? "localhost"}`);
    const path = url.pathname;

    try {
      if (!authorized(req, req.socket.remoteAddress)) {
        throw new GatewayError(401, "Unauthorized. Set Authorization: Bearer <GATEWAY_TOKEN> or bind to loopback.");
      }

      if (path === "/healthz") {
        return send(res, 200, {
          ok: true,
          name: opts.name,
          version: opts.version,
          tokenRequired: Boolean(opts.token),
          ...opts.health,
        });
      }

      if (path === "/tools" && req.method === "GET") {
        return send(res, 200, {
          tools: [...tools.values()].map((t) => ({
            name: t.name,
            description: t.description,
            inputSchema: shapeToJsonSchema(t.schema),
          })),
        });
      }

      if (path === "/rpc" && req.method === "POST") {
        const payload = (await readJson(req)) as { name?: string; args?: Record<string, unknown> };
        const tool = payload.name ? tools.get(payload.name) : undefined;
        if (!tool) {
          throw new GatewayError(400, `Unknown tool ${JSON.stringify(payload.name)}. See GET /tools.`);
        }
        const result = await tool.run(payload.args ?? {});
        return send(res, 200, { ok: true, result });
      }

      if (prefix && path.startsWith(prefix + "/")) {
        const method = req.method ?? "GET";
        const upstreamPath = path.slice(prefix.length) + url.search;
        const body = ["GET", "HEAD"].includes(method) ? undefined : await readJson(req);
        const query = Object.fromEntries(url.searchParams);
        const result = await opts.client.request(method, upstreamPath, { query, body });
        return send(res, 200, result ?? { ok: true });
      }

      throw new GatewayError(404, `No route ${req.method} ${path}. Try /healthz, /tools, /rpc${prefix ? ` or ${prefix}/<api-path>` : ""}.`);
    } catch (err) {
      const { status, message } = errorMessage(err);
      const problem = (err as { problem?: { current?: unknown } }).problem;
      if (problem && problem.current !== undefined) {
        return send(res, status, { error: message, current: problem.current });
      }
      return send(res, status, { error: message });
    }
  });

  server.listen(opts.port, opts.bind, () => {
    console.log(`${opts.name} HTTP listening on http://${opts.bind}:${opts.port}`);
  });
}
