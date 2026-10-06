import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";

/**
 * Offline smoke test: boots the HTTP gateway with a fake session cookie and
 * exercises every route. The upstream (Forge) is intentionally unreachable
 * assumptions-free: passthrough errors must surface as clean JSON errors.
 */
test("http gateway smoke", async (t) => {
  const port = 7800 + (process.pid % 500);
  const base = `http://127.0.0.1:${port}`;
  const child = spawn(process.execPath, ["dist/forge/http.js"], {
    env: {
      ...process.env,
      FORGE_COOKIE: "smoke=test",
      GATEWAY_BIND: "127.0.0.1",
      GATEWAY_PORT: String(port),
    },
    stdio: ["ignore", "inherit", "inherit"],
  });
  t.after(() => child.kill("SIGTERM"));

  let up = false;
  for (let i = 0; i < 50 && !up; i++) {
    await new Promise((r) => setTimeout(r, 100));
    try {
      up = (await fetch(`${base}/healthz`)).ok;
    } catch {
      /* not up yet */
    }
  }
  assert.ok(up, "server did not start");

  const health = await (await fetch(`${base}/healthz`)).json();
  assert.equal(health.ok, true);
  assert.equal(health.auth, "cookie");

  const tools = await (await fetch(`${base}/tools`)).json();
  assert.ok(tools.tools.some((t) => t.name === "forge_request"));

  const unknown = await (
    await fetch(`${base}/rpc`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: "nope" }),
    })
  ).json();
  assert.match(unknown.error, /Unknown tool/);

  const blocked = await (
    await fetch(`${base}/rpc`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: "forge_request", args: { method: "GET", path: "/api/v1/auth/logout" } }),
    })
  ).json();
  assert.match(blocked.error, /blocked for agents/);

  const badPath = await (
    await fetch(`${base}/rpc`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name: "forge_request", args: { method: "GET", path: "/nope" } }),
    })
  ).json();
  assert.match(badPath.error, /must start with \/api\/v1\//);

  // Passthrough to an unreachable upstream must fail cleanly, not crash.
  const passthrough = await (await fetch(`${base}/forge/entities`)).json();
  assert.ok(passthrough.error, "passthrough error should be surfaced as JSON");
});
