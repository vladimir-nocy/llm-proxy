import { ApiClient } from "../core/client.js";
import { GatewayError, UpstreamError, parseProblem } from "../core/errors.js";
import type { ForgeConfig } from "./config.js";

/**
 * Forge's auth model: POST /auth/login sets a session cookie; mutations
 * must carry X-Forge-Csrf. Auth-critical routes are blocked for agents so
 * a tool call can never log the gateway out or mint invites.
 */
const BLOCKED = [/^\/auth\/login/, /^\/auth\/logout/, /^\/auth\/invites/];

export class ForgeClient extends ApiClient {
  constructor(cfg: ForgeConfig) {
    super({
      baseUrl: cfg.url,
      basePath: "/api/v1",
      staticCookie: cfg.cookie,
      sessionFile: cfg.sessionFile,
      headers: (method) => (/^GET$/i.test(method) ? {} : { "x-forge-csrf": "1" }) as Record<string, string>,
      login:
        cfg.user && cfg.password
          ? () => this.doLogin(cfg)
          : undefined,
    });
  }

  private async doLogin(cfg: ForgeConfig): Promise<string> {
    const res = await fetch(`${cfg.url}/api/v1/auth/login`, {
      method: "POST",
      headers: { "content-type": "application/json", "x-forge-csrf": "1" },
      body: JSON.stringify({ user: cfg.user, password: cfg.password }),
    });
    if (!res.ok) throw new UpstreamError(res.status, await parseProblem(res));
    const cookies = res.headers.getSetCookie();
    if (cookies.length === 0) {
      throw new GatewayError(502, "Login succeeded but the server returned no session cookie.");
    }
    return cookies.map((c) => c.split(";")[0]).join("; ");
  }

  override async request(
    method: string,
    path: string,
    opts: { query?: Record<string, unknown>; body?: unknown } = {},
  ): Promise<unknown> {
    if (BLOCKED.some((re) => re.test(path))) {
      throw new GatewayError(403, `Forge auth routes (login/logout/invites) are blocked for agents.`);
    }
    return super.request(method, path, opts);
  }
}
