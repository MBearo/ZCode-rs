import assert from "node:assert/strict";
import test from "node:test";
import { once } from "node:events";
import { createServer } from "node:https";
import { execFile } from "node:child_process";
import { promisify } from "node:util";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { fixture, event, end } from "./rust-agent-fixture.js";

test("Rust retries socket failures, server errors and SSE network errors within one configured budget", async () => {
  const f = await fixture({
    env: { ZCODE_MODEL_RETRY_MAX_RETRIES: "0" },
    config: { retry: { maxRetries: 3, baseDelayMs: 1, maxDelayMs: 1, jitter: false } },
    respond(_req, res, attempt) {
      if (attempt === 1) {
        res.destroy();
        return;
      }
      if (attempt === 2) {
        res.writeHead(503);
        res.end('{"error":{"code":"500"}}');
        return;
      }
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      if (attempt === 3) {
        res.end('data: {"error":{"code":"ECONNRESET","message":"private diagnostic"}}\n\n');
        return;
      }
      event(res, { content: "recovered" });
      end(res, "stop");
    },
  });
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "retry" }));
    await h.completed(id);
    assert.equal(f.requests.length, 4);
    assert.equal(new Set(f.requestBodies).size, 1);
    const retries = h.messages
      .flatMap((m) => m.params?.frame?.payload?.deltas ?? [])
      .map((d: any) => d.patch?.control?.apiRetry)
      .filter(Boolean);
    assert.deepEqual(
      retries.map((r: any) => r.reasonCode),
      ["network_error", "server_error", "network_error"],
    );
    assert(!JSON.stringify(h.messages).includes("private diagnostic"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test(
  "Rust TLS certificate failure is terminal and redacted",
  { skip: process.platform === "win32" },
  async () => {
    const temp = await mkdtemp(join(tmpdir(), "rust-tls-"));
    const key = join(temp, "key.pem"),
      cert = join(temp, "cert.pem");
    // 测试期生成自签名证书；不在仓库保存私钥，也不请求外部供应商。
    await promisify(execFile)("openssl", [
      "req",
      "-x509",
      "-newkey",
      "rsa:2048",
      "-nodes",
      "-keyout",
      key,
      "-out",
      cert,
      "-days",
      "1",
      "-subj",
      "/CN=localhost",
    ]);
    let handshakes = 0;
    const server = createServer({ key: await readFile(key), cert: await readFile(cert) });
    server.on("tlsClientError", () => {
      handshakes++;
    });
    server.listen(0, "127.0.0.1");
    await once(server, "listening");
    const address = server.address();
    assert(address && typeof address !== "string");
    const f = await fixture();
    try {
      const config = JSON.parse(await readFile(f.config, "utf8"));
      await writeFile(
        f.config,
        JSON.stringify({
          ...config,
          baseUrl: `https://127.0.0.1:${address.port}/v1`,
          retry: { maxRetries: 2, baseDelayMs: 1, jitter: false },
        }),
      );
      const h = f.start(),
        id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "tls" }));
      const failure = await h.wait((m) =>
        m.params?.frame?.payload?.deltas?.some((d: any) => d.patch?.control?.phase === "error"),
      );
      const error = failure.params.frame.payload.deltas.find(
        (d: any) => d.patch?.control?.lastError,
      ).patch.control.lastError;
      assert.equal(error.attribution.reason, "tls_error");
      assert.equal(error.attribution.retryable, false);
      assert.equal(handshakes, 1);
      assert(!JSON.stringify(h.messages).includes(String(address.port)));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
      server.closeAllConnections();
      await new Promise<void>((r) => server.close(() => r()));
      await rm(temp, { recursive: true, force: true });
    }
  },
);
