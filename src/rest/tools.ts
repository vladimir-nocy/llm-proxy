import { z, type ZodRawShape } from "zod";
import { GatewayError } from "../core/errors.js";
import type { ToolDef } from "../core/types.js";
import type { RestClient } from "./client.js";

const JSONValue: z.ZodTypeAny = z.union([z.string(), z.number(), z.boolean(), z.null(), z.record(z.unknown()), z.array(z.unknown())]);
const Query: z.ZodRawShape = {
  query: z.record(z.string()).optional().describe("query params, e.g. {\"page\":\"2\"}"),
};

/** Generic tools that work against ANY API — no Forge (or any product) required. */
export function buildRestTools(client: RestClient): ToolDef[] {
  const assertPath = (path: unknown) => {
    if (typeof path !== "string" || !path.startsWith("/")) {
      throw new GatewayError(400, "path must start with / (it is appended to the adapter's base URL)");
    }
    return path;
  };

  return [
    {
      name: "rest_request",
      description:
        "Call any endpoint of the configured API: method, path, query and JSON body. Use this when rest_get/rest_post don't fit (PATCH, PUT, DELETE...).",
      schema: {
        method: z.enum(["GET", "POST", "PATCH", "PUT", "DELETE"]),
        path: z.string().describe("endpoint path starting with /, e.g. /repos/facebook/react"),
        ...Query,
        body: JSONValue.optional().describe("JSON request body (POST/PATCH/PUT)"),
      },
      run: async (a) =>
        client.request(a.method as string, assertPath(a.path), {
          query: a.query as Record<string, string> | undefined,
          body: a.body,
        }),
    },
    {
      name: "rest_get",
      description: "GET any endpoint of the configured API.",
      schema: {
        path: z.string().describe("endpoint path starting with /"),
        ...Query,
      },
      run: async (a) =>
        client.request("GET", assertPath(a.path), { query: a.query as Record<string, string> | undefined }),
    },
    {
      name: "rest_post",
      description: "POST a JSON body to any endpoint of the configured API.",
      schema: {
        path: z.string().describe("endpoint path starting with /"),
        body: JSONValue.optional(),
        ...Query,
      },
      run: async (a) =>
        client.request("POST", assertPath(a.path), {
          query: a.query as Record<string, string> | undefined,
          body: a.body,
        }),
    },
  ];
}
