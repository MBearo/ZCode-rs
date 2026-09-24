import assert from "node:assert/strict";
import test from "node:test";
import type { ServerResponse } from "node:http";
import { zcodeSessionEventSchema, zcodeSessionSubscribeResultSchema } from "@zcode/shared";
import { end, event, fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

/** Streams `text`, then breaks the stream with a network error. */
function breakAfter(res: ServerResponse, text: string) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, { content: text });
  res.end('data: {"error":{"code":"ECONNRESET","message":"socket hang up"}}\n\n');
}

function answer(res: ServerResponse, text: string) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, { content: text });
  end(res, "stop");
}

/** V4 `control.apiRetry` values in publication order. */
function retries(h: Harness, id: string): (Message | null)[] {
  return h.messages
    .filter((m) => m.params?.topic === `conversation/${id}`)
    .flatMap((m) => m.params.frame?.payload?.deltas ?? [])
    .filter((d: Message) => d.op === "state.updated" && d.patch?.control)
    .map((d: Message) => d.patch.control.apiRetry ?? null);
}

async function failed(h: Harness, id: string, after: number) {
  return h.wait(
    (m) =>
      m.params?.topic === `conversation/${id}` &&
      m.params.frame?.payload?.deltas?.some((d: Message) => d.patch?.control?.phase === "error"),
    after,
  );
}

test("Rust recovers a stream that breaks after visible text like Node", async () => {
  const f = await fixture({
    respond(_req, res) {
      if (f.requests.length === 1) breakAfter(res, "partial ");
      else answer(res, "recovered");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const subscribed = await h.client.request(
      "session/subscribe",
      { sessionId: id, deliveryKind: "desktop-continuous" },
      zcodeSessionSubscribeResultSchema,
    );
    assert.equal(subscribed.eventSeq, 0);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "go" }));
    await h.completed(id, after);
    assert.equal(f.requests.length, 2);
    assert.deepEqual(f.requests[1]!.messages, f.requests[0]!.messages, "same history");

    const rows = (await h.rows(id)).rows.filter((r: Message) => r.kind === "assistantText");
    assert.deepEqual(
      rows.map((r: Message) => [r.text, r.state]),
      [
        ["partial ", "interrupted"],
        ["recovered", "complete"],
      ],
    );
    const states = retries(h, id);
    assert.deepEqual(
      states.find((s) => s !== null),
      {
        attempt: 1,
        maxAttempts: 11,
        nextRetryAt: states.find((s) => s !== null)!.nextRetryAt,
        reasonCode: "fault.network.unreachable",
      },
    );
    assert.equal(states.at(-1), null, "cleared after the recovered response");

    // turn.started 带 Node 的额外键（D16），这里只校验本用例关心的事件。
    const sent = h.messages
      .slice(after)
      .filter((m) => m.method === "session/event" && m.params.type !== "turn.started")
      .map((m) => zcodeSessionEventSchema.parse(m.params) as Message);
    const recovery = sent.filter((e) => e.type === "streamRecovery.updated");
    assert.equal(recovery.length, 4);
    const [started, anchor, tail, retried] = recovery.map((e) => e.payload);
    const response = started.assistantMessageId;
    assert.equal(started.attemptId, `${response}:end-of-stream`);
    assert.deepEqual(
      [started.failureKind, started.retryNumber, started.maxRetries],
      ["provider_network_error", 1, 10],
    );
    assert.equal(started._meta.zcode.apiRetry.attempt, 1);
    assert.deepEqual(anchor, {
      attemptId: started.attemptId,
      anchorId: `${response}:previous-message-anchor`,
      reason: "no_tool_committed",
      committedToolCallIds: [],
    });
    assert.deepEqual(
      [tail.discardedTextBytes, tail.discardedReasoningBytes, tail._meta],
      [8, 0, undefined],
    );
    assert.equal(retried.streamMode, "sse");
    assert.equal(retried.failedRequestId, started.failedRequestId);
    const restarted = sent.find(
      (e) =>
        e.type === "session.updated" &&
        e.payload.type === "model_request_started" &&
        e.payload.streamRecovery,
    )!;
    assert.deepEqual(restarted.payload.streamRecovery, {
      attemptId: started.attemptId,
      anchorId: anchor.anchorId,
      maxRetries: 10,
      retryNumber: 1,
      recoveredFromRequestId: started.failedRequestId,
    });
    assert.equal(restarted.payload._meta.zcode.apiRetry.attempt, 1);
  } finally {
    await f.close();
  }
});

test("Rust stops recovering after 10 attempts", async () => {
  const f = await fixture({
    respond(_req, res) {
      breakAfter(res, "again ");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "go" }));
    await failed(h, id, after);
    assert.equal(f.requests.length, 11);
    const attempts = retries(h, id)
      .filter((s) => s !== null)
      .map((s) => s!.attempt);
    assert.equal(Math.max(...attempts), 10);
  } finally {
    await f.close();
  }
});

test("Rust reports adapter retries with Node's fault reason codes", async () => {
  const f = await fixture({
    config: { retry: { maxRetries: 3, baseDelayMs: 1, maxDelayMs: 1, jitter: false } },
    respond(_req, res, attempt) {
      if (attempt === 1) {
        res.writeHead(503);
        res.end('{"error":{"code":"500"}}');
        return;
      }
      answer(res, "ok");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "go" }));
    await h.completed(id, after);
    const states = retries(h, id).filter((s) => s !== null);
    assert.deepEqual(
      states.map((s) => [s!.attempt, s!.maxAttempts, s!.reasonCode]),
      [[1, 4, "fault.provider.serverError"]],
    );
    assert.equal(retries(h, id).at(-1), null);
  } finally {
    await f.close();
  }
});

test("Rust reports a terminal empty completion like Node", async () => {
  const f = await fixture({
    respond(_req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      res.end('data: {"choices":[{"delta":{},"finish_reason":"stop"}]}\n\ndata: [DONE]\n\n');
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "go" }));
    const frame = await failed(h, id, after);
    const error = frame.params.frame.payload.deltas.find(
      (d: Message) => d.patch?.control?.phase === "error",
    ).patch.control.lastError;
    assert.equal(
      error.message,
      "Model returned no text, no tool calls, and no usage before completing the turn.",
    );
    assert.equal(error.attribution.reason, "empty_model_response");
  } finally {
    await f.close();
  }
});

test("Rust keeps output streamed before a stop in the next turn's history like Node", async () => {
  let streaming: () => void;
  const started = new Promise<void>((resolve) => (streaming = resolve));
  const f = await fixture({
    async respond(_req, res) {
      if (f.requests.length === 1) {
        res.writeHead(200, { "content-type": "text/event-stream" });
        event(res, { reasoning_content: "thinking " });
        event(res, { content: "partial answer" });
        streaming();
        await new Promise((resolve) => setTimeout(resolve, 2_000));
        if (!res.destroyed) end(res, "stop");
      } else answer(res, "next");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "first" }));
    await started;
    await h.wait(
      (m) =>
        m.params?.topic === `conversation/${id}` &&
        JSON.stringify(m.params.frame?.payload ?? {}).includes("partial answer"),
      after,
    );
    await h.command(h.envelope("stop", id));
    await h.wait(
      (m) =>
        m.params?.topic === `conversation/${id}` &&
        m.params.frame?.payload?.deltas?.some(
          (d: Message) => d.patch?.control?.phase === "completedInterrupted",
        ),
      after,
    );
    const next = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "second" }));
    await h.completed(id, next);
    const messages = f.requests.at(-1)!.messages.filter((m: Message) => m.role !== "system");
    const turns = messages.map((m: Message) => [m.role, m.content]);
    assert.deepEqual(turns.slice(-3), [
      ["user", turns.at(-3)![1]],
      ["assistant", "partial answer"],
      ["user", turns.at(-1)![1]],
    ]);
    assert.match(JSON.stringify(turns.at(-3)![1]), /first/);
    assert.equal(messages.at(-2).reasoning_content, "thinking ");
  } finally {
    await f.close();
  }
});
