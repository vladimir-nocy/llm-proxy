import { z, type ZodRawShape } from "zod";
import { GatewayError } from "../core/errors.js";
import type { ToolDef } from "../core/types.js";
import type { ForgeClient } from "./client.js";

const JSONValue: z.ZodTypeAny = z.union([z.string(), z.number(), z.boolean(), z.null(), z.record(z.unknown()), z.array(z.unknown())]);

/**
 * Curated tools over Forge's REST API. All routes below are the ones the
 * Forge frontend itself uses (verified from its bundle); `forge_request`
 * covers everything else under /api/v1.
 */
export function buildForgeTools(client: ForgeClient): ToolDef[] {
  const get = (path: string, query?: Record<string, unknown>) => client.request("GET", path, { query });
  const post = (path: string, body?: unknown) => client.request("POST", path, { body });

  return [
    {
      name: "forge_status",
      description:
        "Check the gateway's Forge session: who am I signed in as. Call this first if other tools fail with auth errors.",
      schema: {},
      run: async () => get("/auth/me"),
    },
    {
      name: "forge_request",
      description:
        "Generic call to any Forge API route under /api/v1 (e.g. /entities, /runs, /adapters, /budgets, /comments, /snapshots, /battle/*). Use when a curated tool doesn't fit.",
      schema: {
        method: z.enum(["GET", "POST", "PATCH", "PUT", "DELETE"]).describe("HTTP method"),
        path: z
          .string()
          .describe("API path starting with /api/v1, e.g. /entities?type=asset&channel=draft — write the query string directly in the path"),
        body: JSONValue.optional().describe("JSON request body (for POST/PATCH/PUT)"),
      },
      run: async (args) => {
        const method = args.method as string;
        const path = args.path as string;
        if (!path.startsWith("/api/v1/")) {
          throw new GatewayError(400, "path must start with /api/v1/ — the gateway only proxies the Forge API");
        }
        // Reuse the query-string handling in ApiClient by splitting here.
        const [p, qs] = path.slice("/api/v1".length).split("?");
        const query = Object.fromEntries(new URLSearchParams(qs ?? ""));
        return client.request(method, p, { query, body: args.body });
      },
    },
    {
      name: "forge_list_entities",
      description: "List studio entities (assets, workflows, node presets...). Filter by type, channel, search text.",
      schema: {
        type: z.string().describe("entity type, e.g. asset, workflow, node_preset"),
        channel: z.enum(["draft", "approved", "production"]).optional(),
        q: z.string().optional().describe("search text"),
        limit: z.number().int().optional().describe("max results (default 500)"),
      },
      run: async (a) => get("/entities", a),
    },
    {
      name: "forge_get_entity",
      description: "Fetch one entity by id, including its facets and packages.",
      schema: {
        id: z.string(),
        channel: z.enum(["draft", "approved", "production"]).optional(),
      },
      run: async (a) => get(`/entities/${encodeURIComponent(a.id as string)}`, { channel: a.channel }),
    },
    {
      name: "forge_create_entity",
      description: "Create an entity (e.g. a new asset or workflow node) in the studio.",
      schema: {
        type: z.string().describe("entity type"),
        id: z.string().describe("entity id/slug"),
        body: JSONValue.describe("entity body (JSON)"),
        message: z.string().optional().describe("revision message"),
      },
      run: async (a) => post("/entities", { type: a.type, id: a.id, body: a.body, message: a.message }),
    },
    {
      name: "forge_list_runs",
      description: "List workflow runs (generation jobs).",
      schema: { limit: z.number().int().optional() },
      run: async (a) => get("/runs", a),
    },
    {
      name: "forge_get_run",
      description: "Fetch one workflow run with its steps and results.",
      schema: { id: z.string() },
      run: async (a) => get(`/runs/${encodeURIComponent(a.id as string)}`),
    },
    {
      name: "forge_cancel_run",
      description: "Cancel a running workflow run.",
      schema: { id: z.string() },
      run: async (a) => post(`/runs/${encodeURIComponent(a.id as string)}/cancel`, {}),
    },
    {
      name: "forge_list_snapshots",
      description: "List studio snapshots (save points).",
      schema: { limit: z.number().int().optional() },
      run: async (a) => get("/snapshots", a),
    },
    {
      name: "forge_create_snapshot",
      description: "Create a studio snapshot.",
      schema: { body: JSONValue.optional() },
      run: async (a) => post("/snapshots", a.body ?? {}),
    },
    {
      name: "forge_add_comment",
      description: "Add a markdown comment/note on an entity (review feedback, task notes...).",
      schema: {
        entity_id: z.string(),
        body_md: z.string().describe("comment body in markdown"),
        anchor: JSONValue.optional().describe("optional anchor (facet/field the comment refers to)"),
      },
      run: async (a) =>
        post("/comments", {
          subject_type: "entity",
          subject_id: a.entity_id,
          body_md: a.body_md,
          ...(a.anchor !== undefined ? { anchor: a.anchor } : {}),
        }),
    },
    {
      name: "forge_list_adapters",
      description: "List generation adapters (providers) with health, accounts and pools.",
      schema: {},
      run: async () => get("/adapters"),
    },
    {
      name: "forge_adapter_action",
      description: "Invoke an adapter action (provider-specific operation) on behalf of an account.",
      schema: {
        adapter: z.string().describe("adapter id, e.g. fal, openrouter, meshy"),
        action: z.string(),
        input: JSONValue.optional(),
        account: z.string().optional(),
      },
      run: async (a) =>
        post(`/adapters/${encodeURIComponent(a.adapter as string)}/actions/${encodeURIComponent(a.action as string)}`, {
          ...(a.input !== undefined ? { input: a.input } : {}),
          ...(a.account ? { account: a.account } : {}),
        }),
    },
  ];
}
