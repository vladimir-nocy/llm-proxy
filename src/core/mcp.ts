import { McpServer } from "@modelcontextprotocol/sdk/server/mcp.js";
import { StdioServerTransport } from "@modelcontextprotocol/sdk/server/stdio.js";
import { errorMessage } from "./errors.js";
import type { ToolDef } from "./types.js";

/**
 * Run an MCP server over stdio exposing the given tools. This is what
 * Claude Code and Codex CLI connect to.
 */
export async function runMcpServer(opts: {
  name: string;
  version: string;
  tools: ToolDef[];
  /** One-line diagnostics printed to stderr once ready. */
  banner?: string;
}): Promise<void> {
  const server = new McpServer({ name: opts.name, version: opts.version });

  for (const tool of opts.tools) {
    server.registerTool(
      tool.name,
      { title: tool.name, description: tool.description, inputSchema: tool.schema },
      async (args) => {
        try {
          const result = await tool.run(args ?? {});
          return { content: [{ type: "text", text: JSON.stringify(result, null, 2) }] };
        } catch (err) {
          const { message } = errorMessage(err);
          return { isError: true, content: [{ type: "text", text: message }] };
        }
      },
    );
  }

  await server.connect(new StdioServerTransport());
  // MCP owns stdout; keep any diagnostics on stderr.
  if (opts.banner) console.error(opts.banner);
}
