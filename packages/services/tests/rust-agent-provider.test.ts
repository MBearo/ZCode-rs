import assert from "node:assert/strict";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import { fixture, event, end, type Harness } from "./rust-agent-fixture.js";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";

const retry = { maxRetries: 2, baseDelayMs: 1, maxDelayMs: 1, jitter: false };
const patches = (h: Harness) =>
  h.messages
    .flatMap((m) => m.params?.frame?.payload?.deltas ?? [])
    .map((d: any) => d.patch)
    .filter(Boolean);
async function begin(f: Awaited<ReturnType<typeof fixture>>) {
  const h = f.start();
  const id = await h.create();
  await h.subscribe(`conversation/${id}`);
  await h.command(h.envelope("sendText", id, { text: "provider test" }));
  return { h, id };
}
async function failed(h: Harness) {
  await h.wait((m) =>
    m.params?.frame?.payload?.deltas?.some((d: any) => d.patch?.control?.phase === "error"),
  );
  return patches(h).at(-1).control.lastError;
}

test("Rust retries rate limits with identical bytes, reuses HTTP connections and exposes App retry state", async () => {
  const f = await fixture({
    config: { retry },
    respond(_req, res, attempt) {
      if (attempt === 1) {
        res.writeHead(429, { "Content-Type": "application/json", "retry-after-ms": "40" });
        res.end(
          '{"error":{"code":"rate_limit_error","message":"fixture secret must stay private"}}',
        );
      } else {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        event(res, { content: "recovered" });
        end(res, "stop");
      }
    },
  });
  try {
    const { h, id } = await begin(f);
    await h.completed(id);
    assert.equal(f.requests.length, 2);
    assert.equal(f.requestBodies[0], f.requestBodies[1]);
    assert.equal(new Set(f.connectionPorts).size, 1);
    const states = patches(h)
      .map((p: any) => p.control?.apiRetry)
      .filter(Boolean);
    assert.equal(states[0].maxAttempts, 3);
    assert.equal(states[0].reasonCode, "rate_limited");
    assert.equal(patches(h).at(-1).control.apiRetry, null);
    assert(!JSON.stringify(h.messages).includes("fixture secret"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust classifies terminal business errors ahead of generic HTTP retries", async () => {
  for (const [status, code, expected] of [
    [429, "insufficient_quota", "model_rate_limited"],
    [403, "3007", "invalid_model_request"],
    [400, "1261", "model_context_exceeded"],
    [401, "unknown", "provider_not_configured"],
  ] as const) {
    const f = await fixture({
      config: { retry },
      respond(_req, res) {
        res.writeHead(status, { "Content-Type": "application/json" });
        res.end(JSON.stringify({ error: { code, message: "fixture-sensitive-content" } }));
      },
    });
    try {
      const { h, id } = await begin(f);
      assert.equal((await failed(h)).code, expected);
      const snapshot = await h.client.request(
        "session/read",
        { sessionId: id },
        zcodeSessionStateSnapshotSchema,
      );
      assert.equal(snapshot.session.status, "error");
      assert.equal(snapshot.projection.lastError?.code, expected);
      assert.equal(f.requests.length, 1);
      assert(!JSON.stringify(h.messages).includes("fixture-sensitive-content"));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  }
});

test("Rust retries incomplete tool prelude, but never replays committed text or reasoning", async () => {
  for (const kind of ["prelude", "content", "reasoning_content"] as const) {
    const f = await fixture({
      config: { retry },
      async respond(_req, res, attempt) {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        if (attempt === 1) {
          event(
            res,
            kind === "prelude"
              ? {
                  tool_calls: [
                    { index: 0, id: "partial", function: { name: "Write", arguments: '{"path":' } },
                  ],
                }
              : { [kind]: "visible partial" },
          );
          await delay(30);
          res.end(); // Valid HTTP EOF, missing provider completion marker.
        } else {
          event(res, { content: "retry success" });
          end(res, "stop");
        }
      },
    });
    try {
      const { h, id } = await begin(f);
      if (kind === "prelude") {
        await h.completed(id);
        assert.equal(f.requests.length, 2);
        assert(!(await h.rows(id)).rows.some((r) => r.kind === "toolCall"));
      } else {
        await failed(h);
        assert.equal(f.requests.length, 1);
        const row = (await h.rows(id)).rows.find(
          (r) => r.kind === (kind === "content" ? "assistantText" : "reasoning"),
        );
        assert(row?.kind === "assistantText" || row?.kind === "reasoning");
        assert.equal(row?.text, "visible partial");
        assert.equal(row?.state, "interrupted");
      }
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  }
});

test("Rust empty completion retry is capped and usage-only completion is accepted", async () => {
  for (const usage of [false, true]) {
    const f = await fixture({
      config: { retry },
      respond(_req, res) {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        if (usage) end(res, "stop");
        else res.end('data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
      },
    });
    try {
      const { h, id } = await begin(f);
      if (usage) {
        await h.completed(id);
        assert.equal(f.requests.length, 1);
      } else {
        assert.equal((await failed(h)).code, "invalid_model_response");
        assert.equal(f.requests.length, 2);
      }
    } finally {
      await f.close();
    }
  }
});

test("Rust preserves reasoning and tool order across the next request and cold history", async () => {
  const f = await fixture({
    config: { retry },
    respond(req, res) {
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      if (
        req.messages.at(-1).role === "user" &&
        !req.messages.some((m: any) => m.role === "assistant")
      ) {
        event(res, { reasoning_content: "inspect first" });
        event(res, {
          tool_calls: [
            { index: 0, id: "read", function: { name: "Read", arguments: '{"file_path":' } },
            { index: 1, id: "list", function: { name: "List", arguments: '{"path":"."}' } },
          ],
        });
        event(res, { tool_calls: [{ index: 0, function: { arguments: '"missing.txt"}' } }] });
        end(res, "tool_calls");
      } else {
        event(res, { content: "done" });
        end(res, "stop");
      }
    },
  });
  try {
    const { h, id } = await begin(f);
    await h.completed(id);
    const messages = f.requests[1]!.messages;
    assert.equal(
      messages.find((m: any) => m.role === "assistant").reasoning_content,
      "inspect first",
    );
    assert.deepEqual(
      messages.filter((m: any) => m.role === "tool").map((m: any) => m.tool_call_id),
      ["read", "list"],
    );
    assert.match(messages.at(-2).content, /Tool failed/);
    await h.close();
    const recovered = f.start();
    await recovered.subscribe(`conversation/${id}`);
    await recovered.command(recovered.envelope("sendText", id, { text: "continue" }));
    await recovered.completed(id);
    assert.equal(
      f.requests.at(-1)!.messages.find((m: any) => m.role === "assistant").reasoning_content,
      "inspect first",
    );
    assert.deepEqual(recovered.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust stop cancels header wait, idle stream and long Retry-After immediately", async () => {
  for (const mode of ["headers", "idle", "backoff"] as const) {
    const f = await fixture({
      config: { retry, streamIdleTimeoutMs: 1000 },
      respond(_req, res) {
        if (mode === "backoff") {
          res.writeHead(429, { "retry-after": "120" });
          res.end("{}");
        }
        if (mode === "idle") {
          res.writeHead(200, { "Content-Type": "text/event-stream" });
          res.flushHeaders();
        }
      },
    });
    try {
      const { h, id } = await begin(f);
      if (mode === "backoff")
        await h.wait((m) =>
          m.params?.frame?.payload?.deltas?.some((d: any) => d.patch?.control?.apiRetry),
        );
      else for (let i = 0; i < 100 && !f.requests.length; i++) await delay(10);
      const at = performance.now();
      await h.command(h.envelope("stop", id));
      await h.wait((m) =>
        m.params?.frame?.payload?.deltas?.some(
          (d: any) => d.patch?.control?.phase === "completedInterrupted",
        ),
      );
      assert(performance.now() - at < 500);
      assert.equal(patches(h).at(-1).control.apiRetry, null);
      assert.equal(f.requests.length, 1);
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  }
});

test("Rust idle timeout and explicit total timeout recover before output", async () => {
  for (const config of [{ streamIdleTimeoutMs: 40 }, { requestTimeoutSeconds: 1 }]) {
    const f = await fixture({
      config: { retry, ...config },
      respond(_req, res, attempt) {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        res.flushHeaders();
        if (attempt > 1) {
          event(res, { content: "recovered timeout" });
          end(res, "stop");
        }
      },
    });
    try {
      const { h, id } = await begin(f);
      await h.completed(id);
      assert.equal(f.requests.length, 2);
      assert(
        patches(h).some(
          (p: any) =>
            p.control?.apiRetry?.reasonCode ===
            ("streamIdleTimeoutMs" in config ? "stream_idle_timeout" : "timeout"),
        ),
      );
    } finally {
      await f.close();
    }
  }
});

test("Rust flushes coalesced text while provider is idle and preserves mixed reasoning/text order", async () => {
  let finish: (() => void) | undefined;
  let ended = false;
  const f = await fixture({
    config: { retry },
    async respond(_req, res) {
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      event(res, { reasoning_content: "thinking" });
      event(res, { content: "A" });
      event(res, { content: "B" });
      await new Promise<void>((resolve) => {
        finish = resolve;
      });
      ended = true;
      event(res, { reasoning_content: " checking" });
      event(res, { content: "C" });
      end(res, "stop");
    },
  });
  try {
    const { h, id } = await begin(f);
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: any) => d.row?.kind === "assistantText" && d.row.text === "AB",
      ),
    );
    assert.equal(ended, false);
    finish!();
    await h.completed(id);
    const rows = (await h.rows(id)).rows;
    assert.equal(rows.find((r) => r.kind === "assistantText")?.text, "ABC");
    assert.equal(rows.find((r) => r.kind === "reasoning")?.text, "thinking checking");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    finish?.();
    await f.close();
  }
});

test("Rust rejects duplicate and unfinished tool calls without executing them", async () => {
  for (const invalid of ["duplicate", "missing-name", "bad-json", "length"] as const) {
    const f = await fixture({
      config: { retry },
      respond(_req, res) {
        res.writeHead(200, { "Content-Type": "text/event-stream" });
        if (invalid === "bad-json") res.end("data: {broken\n\n");
        else {
          event(res, {
            tool_calls: [
              {
                index: 0,
                id: "id",
                function: {
                  name: invalid === "missing-name" ? "" : "Read",
                  arguments: '{"path":"a"}',
                },
              },
              ...(invalid === "duplicate"
                ? [
                    {
                      index: 1,
                      id: "id",
                      function: { name: "Write", arguments: '{"path":"a","content":"unsafe"}' },
                    },
                  ]
                : []),
            ],
          });
          end(res, invalid === "length" ? "length" : "tool_calls");
        }
      },
    });
    try {
      const { h, id } = await begin(f);
      assert.equal((await failed(h)).code, "invalid_model_response");
      assert.equal(f.requests.length, 1);
      assert(!(await h.rows(id)).rows.some((r) => r.kind === "toolCall"));
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  }
});
