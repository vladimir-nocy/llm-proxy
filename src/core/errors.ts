/** Error shape mirrors RFC-7807 problem+json responses (Forge speaks this). */
export type Problem = {
  type?: string;
  title?: string;
  code?: string;
  detail?: string;
  current?: unknown;
  [k: string]: unknown;
};

/** An error response from the upstream API. */
export class UpstreamError extends Error {
  constructor(
    readonly status: number,
    readonly problem: Problem,
  ) {
    super(problem.detail ?? problem.title ?? `Upstream HTTP ${status}`);
    this.name = "UpstreamError";
  }
}

/** An error raised by the gateway itself (bad input, auth config, ...). */
export class GatewayError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly extra: Record<string, unknown> = {},
  ) {
    super(message);
    this.name = "GatewayError";
  }
}

export async function parseProblem(res: Response): Promise<Problem> {
  try {
    return (await res.json()) as Problem;
  } catch {
    return { title: res.statusText || `HTTP ${res.status}`, status: res.status, code: "http" };
  }
}

/** Format any tool error into a clean message for MCP/HTTP callers. */
export function errorMessage(err: unknown): { status: number; message: string } {
  if (err instanceof UpstreamError) {
    const bits = [err.problem.title ?? `HTTP ${err.status}`];
    if (err.problem.detail) bits.push(err.problem.detail);
    if (err.status === 401) bits.push("The upstream session may have expired — check the gateway's credentials.");
    return { status: err.status === 0 ? 502 : err.status, message: bits.join(" — ") };
  }
  if (err instanceof GatewayError) return { status: err.status, message: err.message };
  return { status: 500, message: err instanceof Error ? err.message : String(err) };
}
