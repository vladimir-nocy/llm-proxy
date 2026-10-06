import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname } from "node:path";
import { GatewayError, UpstreamError, parseProblem } from "./errors.js";

export type ApiClientOptions = {
  /** Upstream base URL, e.g. https://studio.example.com */
  baseUrl: string;
  /** Path prefix for API routes, e.g. "/api/v1". Default: none. */
  basePath?: string;
  /** Pre-shared session cookie (e.g. pasted from a browser). Skips login. */
  staticCookie?: string;
  /** Where the session cookie is cached between restarts. */
  sessionFile?: string;
  /** Extra headers per request (e.g. CSRF tokens on mutations). */
  headers?: (method: string) => Record<string, string>;
  /**
   * Acquire a fresh session; returns the cookie to adopt. Called when no
   * session exists and again (once) after a 401.
   */
  login?: () => Promise<string>;
};

type SessionFile = { cookie: string; savedAt: string };

/**
 * Authenticated HTTP client for a JSON API with cookie-based sessions.
 *
 * - Session cookie is cached to disk so restarts (and separate MCP + HTTP
 *   processes) share one login.
 * - Retries once after re-login on a 401, when a login strategy is set.
 */
export class ApiClient {
  private cookie: string | null = null;
  private loggingIn: Promise<void> | null = null;

  constructor(protected readonly o: ApiClientOptions) {}

  async ensureSession(): Promise<void> {
    if (this.cookie) return;
    if (this.o.staticCookie) {
      this.cookie = this.o.staticCookie;
      return;
    }
    try {
      const raw = JSON.parse(await readFile(this.o.sessionFile!, "utf8")) as SessionFile;
      if (raw.cookie) {
        this.cookie = raw.cookie;
        return;
      }
    } catch {
      /* no cached session */
    }
    await this.login();
  }

  async login(): Promise<void> {
    if (!this.o.login) {
      throw new GatewayError(
        401,
        "No upstream credentials configured. Set the adapter's credentials in .env, or paste a session cookie.",
      );
    }
    // Collapse concurrent logins into one.
    this.loggingIn ??= this.o
      .login()
      .then((cookie) => this.adoptSession(cookie))
      .finally(() => (this.loggingIn = null));
    await this.loggingIn;
  }

  async request(
    method: string,
    path: string,
    opts: { query?: Record<string, unknown>; body?: unknown } = {},
  ): Promise<unknown> {
    const url = new URL(`${this.o.baseUrl.replace(/\/+$/, "")}${this.o.basePath ?? ""}${path}`);
    for (const [k, v] of Object.entries(opts.query ?? {})) {
      if (v !== undefined && v !== null && v !== "") url.searchParams.set(k, String(v));
    }

    const send = async () => {
      await this.ensureSession();
      const headers: Record<string, string> = { cookie: this.cookie!, ...this.o.headers?.(method) };
      if (opts.body !== undefined) headers["content-type"] = "application/json";
      return fetch(url, {
        method,
        headers,
        body: opts.body !== undefined ? JSON.stringify(opts.body) : undefined,
      });
    };

    let res = await send();
    if (res.status === 401 && this.o.login) {
      // Session expired — force a fresh login and retry once.
      this.cookie = null;
      await this.login();
      res = await send();
    }
    if (!res.ok) throw new UpstreamError(res.status, await parseProblem(res));
    if (res.status === 204) return undefined;
    const text = await res.text();
    return text ? JSON.parse(text) : undefined;
  }

  private async adoptSession(cookie: string): Promise<void> {
    this.cookie = cookie;
    if (!this.o.sessionFile) return;
    try {
      await mkdir(dirname(this.o.sessionFile), { recursive: true });
      await writeFile(
        this.o.sessionFile,
        JSON.stringify({ cookie, savedAt: new Date().toISOString() } satisfies SessionFile, null, 2),
      );
    } catch {
      /* best-effort cache */
    }
  }
}
