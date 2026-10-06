import { env, defaultSessionFile } from "../core/env.js";

export type ForgeConfig = {
  /** Base URL of the Forge studio server. */
  url: string;
  /** Login credentials (option A). */
  user?: string;
  password?: string;
  /** Pre-shared session cookie (option B), e.g. "forge_session=abc123". */
  cookie?: string;
  /** Where the session cookie is cached between restarts. */
  sessionFile: string;
};

export function forgeConfigFromEnv(): ForgeConfig {
  return {
    url: (env("FORGE_URL") ?? "http://sumos-macbook-pro.local:7700").replace(/\/+$/, ""),
    user: env("FORGE_USER"),
    password: env("FORGE_PASSWORD"),
    cookie: env("FORGE_COOKIE"),
    sessionFile: env("FORGE_SESSION_FILE") ?? defaultSessionFile("forge"),
  };
}
