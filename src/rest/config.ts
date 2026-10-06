import { env } from "../core/env.js";

export type RestConfig = {
  /** Base URL of the target API, e.g. https://api.github.com */
  baseUrl: string;
  /** Optional path prefix prepended to every call, e.g. /api/v1 */
  basePath?: string;
  /** Bearer token → sent as `Authorization: Bearer <token>`. */
  token?: string;
  /** API key → sent in REST_API_KEY_HEADER (default `x-api-key`). */
  apiKey?: string;
  apiKeyHeader?: string;
  /** Extra static headers (JSON object from REST_HEADERS). */
  headers?: Record<string, string>;
};

export function restConfigFromEnv(): RestConfig {
  let headers: Record<string, string> | undefined;
  const raw = env("REST_HEADERS");
  if (raw) {
    try {
      headers = JSON.parse(raw) as Record<string, string>;
    } catch {
      throw new Error(`REST_HEADERS must be a JSON object, e.g. {"x-team":"billy-boys"}`);
    }
  }
  return {
    baseUrl: (env("REST_BASE_URL") ?? "").replace(/\/+$/, ""),
    basePath: env("REST_BASE_PATH"),
    token: env("REST_TOKEN"),
    apiKey: env("REST_API_KEY"),
    apiKeyHeader: env("REST_API_KEY_HEADER"),
    headers,
  };
}
