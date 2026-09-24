import assert from "node:assert/strict";
import test from "node:test";
import { sessionDebugSnapshotSchema, zcodeSessionEventSchema } from "@zcode/shared";
import { end, event, fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

function statuses(h: Harness, after: number) {
  return h.messages
    .slice(after)
    .filter((m) => m.method === "session/event" && m.params.payload?.requestId)
    .map((m) => zcodeSessionEventSchema.parse(m.params) as Message);
}
async function run(h: Harness, id: string, text: string) {
  const before = h.messages.length;
  await h.command(h.envelope("sendText", id, { text }));
  await h.completed(id, before);
  return before;
}
function debug(h: Harness, sessionId: string) {
  return h.client.request("session/debug", { sessionId }, sessionDebugSnapshotSchema);
}

test("Rust records model requests for session/debug and the legacy stream like Node", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.client.request("session/subscribe", {
      sessionId: id,
      deliveryKind: "web-remote-replayable",
    });
    const before = await run(h, id, "hello");
    const sent = statuses(h, before);
    assert.deepEqual(
      sent.map((e) => e.payload.type),
      ["model_request_started", "model_request_completed"],
    );
    const [started, completed] = sent;
    assert.deepEqual(started!.payload._meta, { zcode: { apiRetry: null } });
    assert.deepEqual(completed!.payload._meta, { zcode: { apiRetry: null } });
    assert.equal(started!.payload.querySource, "main_turn");
    assert.equal(started!.payload.attempt, 1);
    assert.equal(started!.payload.turnId, started!.turnId);
    // 状态视图的 x-request-id 就是发出的请求头；由 key 生成的鉴权头不在视图中。
    const headers = started!.payload.requestHeaders;
    assert.equal(headers["x-request-id"], f.requestHeaders[0]!["x-request-id"]);
    assert.equal(headers.authorization, undefined);
    assert.equal(started!.payload.requestHeaderCount, Object.keys(headers).length);
    assert.equal(completed!.payload.finishReason, "stop");
    assert.deepEqual(completed!.payload.usage, {
      inputTokens: 10,
      outputTokens: 4,
      totalTokens: 14,
    });
    const snapshot = await debug(h, id);
    assert.deepEqual(
      snapshot.networkEntries.map((e) => e.statusType),
      ["model_request_started", "model_request_completed"],
    );
    assert.equal(snapshot.rounds.length, 1);
    assert.equal(snapshot.rounds[0]!.requestId, started!.payload.requestId);
    assert.deepEqual(snapshot.rounds[0]!.usage, {
      inputTokens: 10,
      outputTokens: 4,
      totalTokens: 14,
    });
    // 提供方未报告缓存读数：命中率未知。
    assert.deepEqual(snapshot.cache, {
      hitRateRequestCount: 1,
      totalInputTokens: 10,
      totalCacheReadTokens: 0,
      hitRate: null,
    });
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust reports a retried request with Node's failed, retry and api retry meta", async () => {
  const f = await fixture({
    async respond(_request, res, count) {
      if (count === 1) {
        res.writeHead(429, { "content-type": "application/json", "retry-after-ms": "5" });
        res.end(
          JSON.stringify({ error: { code: "1302", message: "busy  now", request_id: "p1" } }),
        );
        return;
      }
      res.writeHead(200, { "Content-Type": "text/event-stream" });
      event(res, { content: "ok" });
      end(res, "stop");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const before = await run(h, id, "hello");
    // 未订阅 legacy 流也记录 session/debug。
    assert.deepEqual(statuses(h, before), []);
    const snapshot = await debug(h, id);
    const entries = snapshot.networkEntries;
    assert.deepEqual(
      entries.map((e) => e.statusType),
      [
        "model_request_started",
        "model_request_failed",
        "model_retry_scheduled",
        "model_request_started",
        "model_request_completed",
      ],
    );
    const [first, failed, retry, second] = entries;
    assert.equal(failed!.retryable, true);
    assert.equal(failed!.statusCode, 429);
    assert.equal(failed!.message, "busy now");
    assert.equal(failed!.reason, "rate_limited");
    assert.equal(retry!.delayMs, 5);
    assert.equal(retry!.nextAttempt, 2);
    assert.equal(second!.attempt, 2);
    assert.notEqual(first!.requestId, second!.requestId);
    assert.equal(snapshot.rounds.length, 1);
    await h.client.request("session/subscribe", {
      sessionId: id,
      deliveryKind: "desktop-continuous",
    });
    const replay = await run(h, id, "again");
    const sent = statuses(h, replay);
    assert.equal(sent[0]!.payload.type, "model_request_started");
  } finally {
    await f.close();
  }
});

test("Rust session/debug rejects like Node", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const missing: Message = await debug(h, "sess_gone").catch((e) => e);
    assert.equal(missing.code, -32004);
    assert.equal(missing.message, "Session is not active: sess_gone");
    const invalid = (await h.client.request("session/debug", {}).catch((e) => e)) as Message;
    assert.equal(invalid.code, -32603);
    assert.equal(invalid.data.name, "ZodError");
    assert.equal(JSON.parse(invalid.message)[0].path[0], "sessionId");
  } finally {
    await f.close();
  }
});
