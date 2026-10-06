import { test } from "node:test";
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { createServer } from "node:http";

/**
 * REST adapter end-to-end against a local fixture API (offline, CI-safe):
 * tool calls, auth header injection, passthrough, error mapping, path guard.
 */
test("rest adapter end-to-end", async (t) => {
  const upstream = createServer((req, res) => {
    let body = "";
    req.on("data", (c) => (body += c));
    req.on("end", () => {
      if (req.url === "/ping") {
        res.writeHead(200, { "content-type": "application/json" });
        res.end(JSON.stringify({ pong: true, auth: req.headers.authorization ?? null }));
      } else if (req.url === "/echo" && req.method === "POST") {
        res.writeHead(201, { "content-type": "application/json" });
        res.end(JSON.stringify({ got: JSON.parse(body || "null") }));
      } else {
        res.writeHead(404, { "content-type": "application/json" });
        res.end(JSON.stringify({ title: "Not Found", status: 404 }));
      }
    });
  });
  await new Promise((r) => upstream.listen(0, "127.0.0.1", r));
  const upstreamPort = upstream.address().port;
  t.after(() => upstream.close());

  const gwPort = 7900 + (process.pid % 100);
  const base = `http://127.0.0.1:${gwPort}`;
  const child = spawn(process.execPath, ["dist/rest/http.js"], {
    env: {
      ...process.env,
      REST_BASE_URL: `http://127.0.0.1:${upstreamPort}`,
      REST_TOKEN: "t0k",
      GATEWAY_PORT: String(gwPort),
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
  assert.ok(up, "gateway did not start");

  const health = await (await fetch(`${base}/healthz`)).json();
  assert.equal(health.base, `http://127.0.0.1:${upstreamPort}`);
  assert.equal(health.auth, "bearer");

  const rpc = (name, args) =>
    fetch(`${base}/rpc`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ name, args }),
    }).then((r) => r.json());

  const ping = await rpc("rest_get", { path: "/ping" });
  assert.equal(ping.ok, true);
  assert.equal(ping.result.pong, true);
  assert.equal(ping.result.auth, "Bearer t0k", "bearer token must be attached");

  const echo = await rpc("rest_post", { path: "/echo", body: { hello: "world" } });
  assert.deepEqual(echo.result.got, { hello: "world" });

  const nf = await rpc("rest_get", { path: "/nope" });
  assert.match(nf.error, /Not Found/);

  const badPath = await rpc("rest_get", { path: "http://evil.example" });
  assert.match(badPath.error, /must start with \//);

  const pt = await (await fetch(`${base}/rest/ping`)).json();
  assert.equal(pt.pong, true, "passthrough works");
});
