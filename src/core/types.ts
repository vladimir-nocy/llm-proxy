import type { ZodRawShape } from "zod";

/** A gateway tool, exposed identically over MCP and HTTP. */
export type ToolDef = {
  name: string;
  description: string;
  schema: ZodRawShape;
  run: (args: Record<string, unknown>) => Promise<unknown>;
};
