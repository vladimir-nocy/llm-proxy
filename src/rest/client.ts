import { ApiClient } from "../core/client.js";
import type { RestConfig } from "./config.js";

/**
 * Generic client for any JSON API. Auth is whatever you configured:
 * bearer token, API-key header, custom headers — or nothing at all
 * for open APIs. No cookie session unless the API sets one itself.
 */
export class RestClient extends ApiClient {
  constructor(cfg: RestConfig) {
    super({
      baseUrl: cfg.baseUrl,
      basePath: cfg.basePath,
      headers: () => {
        const h: Record<string, string> = { ...cfg.headers };
        if (cfg.token) h.authorization = `Bearer ${cfg.token}`;
        if (cfg.apiKey) h[(cfg.apiKeyHeader ?? "x-api-key").toLowerCase()] = cfg.apiKey;
        return h;
      },
    });
  }
}
