import assert from "node:assert/strict";
import test from "node:test";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fixture, event, end, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
/** Node 的摘要请求：末条 user 消息是 buildCompactPrompt 的全文。 */
const summaryRequest = (req: Message) =>
  String(req.messages.at(-1).content).startsWith("CRITICAL: Respond with TEXT ONLY");
function reply(res: Parameters<typeof event>[0], content: string) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, { content });
  end(res, "stop");
}
async function send(h: Harness, id: string, text: string) {
  const after = h.messages.length;
  const ack = await h.command(h.envelope("sendText", id, { text }));
  assert.equal(ack.status, "accepted");
  await h.completed(id, after);
}
async function terminal(h: Harness, id: string, phase: string, after: number) {
  return h.wait(
    (m) =>
      m.params?.topic === `conversation/${id}` &&
      m.params.frame?.payload?.deltas?.some((d: Message) => d.patch?.control?.phase === phase),
    after,
  );
}

test("Rust manual compact keeps full history, hides summary stream and restores the durable boundary", async () => {
  const f = await fixture({
    respond(req, res) {
      reply(
        res,
        summaryRequest(req) ? "COMPACT_SUMMARY preserve the original constraints" : "normal answer",
      );
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "original question");
    const command = h.envelope("compact", id);
    const after = h.messages.length;
    assert.equal((await h.command(command)).status, "accepted");
    await h.completed(id, after);
    assert.equal((await h.command(command)).status, "duplicate");
    assert.equal(f.requests.length, 2);
    // Node：摘要请求带与 agent step 相同的 system 前缀和工具定义。
    const summary = f.requests[1]!;
    assert.equal(summary.messages[0].role, "system");
    assert.deepEqual(summary.tools, f.requests[0]!.tools);
    assert.equal(summary.max_tokens, 20000);
    const rows = (await h.rows(id)).rows;
    assert.equal(rows.filter((r) => r.kind === "userInput").length, 1);
    assert(
      rows.some(
        (r) =>
          r.kind === "timelineMarker" &&
          r.marker.type === "compact" &&
          r.marker.status === "success",
      ),
    );
    assert(!JSON.stringify(rows).includes("COMPACT_SUMMARY"));
    await h.close();
    await writeFile(join(f.cwd, "AGENTS.md"), "FRESH_RULE preserve all tests.");
    const resumed = f.start();
    await resumed.subscribe(`conversation/${id}`);
    await send(resumed, id, "next question");
    const request = f.requests.at(-1)!;
    assert(
      request.messages.some((m: Message) => m.role === "user" && m.content.includes("FRESH_RULE")),
    );
    const compacted = request.messages.find(
      (m: Message) => m.role === "user" && m.content.includes("COMPACT_SUMMARY"),
    );
    assert.equal(
      compacted.content,
      [
        "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.",
        "",
        "COMPACT_SUMMARY preserve the original constraints",
        'Continue the conversation from where it left off without asking the user any further questions. Resume directly — do not acknowledge the summary, do not recap what was happening, do not preface with "I\'ll continue" or similar. Pick up the last task as if the break never happened.',
      ].join("\n"),
    );
    assert(!request.messages.some((m: Message) => m.content === "original question"));
    assert(
      (await resumed.rows(id)).rows.some(
        (r) => r.kind === "userInput" && r.text === "original question",
      ),
    );
    assert.deepEqual([...h.schemaErrors, ...resumed.schemaErrors], []);
  } finally {
    await f.close();
  }
});

test("Rust queues compact in FIFO and projects maintenance without a user message", async () => {
  const f = await fixture({
    async respond(req, res, attempt) {
      if (attempt === 1) await delay(120);
      reply(res, summaryRequest(req) ? "queued summary" : "answer");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "first" }));
    const compact = await h.command(h.envelope("compact", id));
    assert.equal((compact.result as Message).delivery, "queue");
    const next = h.envelope("sendText", id, { text: "after compact" });
    await h.command(next);
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: Message) =>
          d.row?.kind === "turnHeader" &&
          d.row?.sourceCommandId === next.commandId &&
          d.row?.state === "completedSuccess",
      ),
    );
    assert.equal(f.requests.length, 3);
    assert(summaryRequest(f.requests[1]!));
    assert(JSON.stringify(f.requests[2]).includes("queued summary"));
    assert.equal((await h.rows(id)).rows.filter((r) => r.kind === "userInput").length, 2);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust cancelling an in-flight summary leaves the old context intact", async () => {
  let started!: () => void;
  const summarizing = new Promise<void>((r) => {
    started = r;
  });
  const f = await fixture({
    async respond(req, res) {
      if (summaryRequest(req)) {
        res.writeHead(200, { "content-type": "text/event-stream" });
        event(res, { content: "partial hidden summary" });
        started();
        await delay(500);
        if (!res.destroyed) end(res, "stop");
      } else reply(res, "answer");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "original before cancellation");
    const after = h.messages.length;
    await h.command(h.envelope("compact", id));
    await summarizing;
    await h.command(h.envelope("stop", id));
    await terminal(h, id, "completedInterrupted", after);
    const rows = (await h.rows(id)).rows;
    assert(
      rows.some(
        (r) =>
          r.kind === "timelineMarker" &&
          r.marker.type === "compact" &&
          r.marker.status === "cancelled",
      ),
    );
    assert(!JSON.stringify(rows).includes("partial hidden summary"));
    await send(h, id, "continue");
    assert(
      f.requests
        .at(-1)!
        .messages.some((m: Message) => m.content === "original before cancellation"),
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust automatic and reactive compaction preserve the last round and complete after commit", async () => {
  for (const reactive of [false, true]) {
    let turns = 0;
    const f = await fixture({
      config: reactive
        ? {}
        : // Agent/SendMessage 定义也计入上下文；首轮需容纳完整工具，长回复仍须触发压缩。
          { contextWindow: 18000, maxOutputTokens: 1000, contextBufferTokens: 2000 },
      respond(req, res) {
        if (summaryRequest(req)) return reply(res, "small durable summary");
        turns += 1;
        // 反应式：第三轮首个请求报上下文超限，压缩后重发。
        if (reactive && turns === 3) {
          res.writeHead(400, { "content-type": "application/json" });
          res.end('{"error":{"code":"context_length_exceeded"}}');
          return;
        }
        reply(res, turns === 1 ? "history".repeat(5000) : "continued");
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await send(h, id, "first");
      await send(h, id, "second");
      await send(h, id, "current input");
      const last = f.requests.at(-1)!;
      assert(f.requests.some(summaryRequest));
      // Node 按 assistant 开始分组，保留最后一组（上一条回复与当前输入）。
      assert.deepEqual(
        last.messages.slice(-2).map((m: Message) => [m.role, m.content]),
        [
          ["assistant", "continued"],
          ["user", "current input"],
        ],
      );
      assert(!JSON.stringify(last).includes("historyhistory"));
      assert.equal(last.max_tokens, reactive ? 32000 : 1000);
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  }
});

test("Rust keeps sending requests when automatic compaction fails and stops trying after 3 failures", async () => {
  let summaries = 0;
  const f = await fixture({
    config: { contextWindow: 18000, maxOutputTokens: 1000, contextBufferTokens: 2000 },
    respond(req, res) {
      if (summaryRequest(req)) {
        summaries += 1;
        res.writeHead(401, { "content-type": "application/json" });
        res.end('{"error":{"code":"unauthorized"}}');
        return;
      }
      reply(res, "history".repeat(3000));
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    for (const text of ["one", "two", "three", "four", "five", "six"]) {
      await send(h, id, text);
    }
    assert.equal(summaries, 3, "the circuit breaker skips automatic compaction after 3 failures");
    const markers = (await h.rows(id)).rows.filter(
      (r) => r.kind === "timelineMarker" && r.marker.type === "compact",
    );
    assert.deepEqual(
      markers.map((r) => (r as Message).marker.status),
      ["failed", "failed", "failed"],
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust failed compaction keeps old context and failed marker across restart", async () => {
  const f = await fixture({
    respond(req, res) {
      if (summaryRequest(req)) {
        res.writeHead(401, { "content-type": "application/json" });
        res.end('{"error":{"code":"unauthorized"}}');
      } else reply(res, "answer");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "keep this original");
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "/compact focus on tests" }));
    await terminal(h, id, "error", after);
    assert(
      (await h.rows(id)).rows.some(
        (r) =>
          r.kind === "timelineMarker" &&
          r.marker.type === "compact" &&
          r.marker.status === "failed",
      ),
    );
    await h.close();
    const resumed = f.start();
    await resumed.subscribe(`conversation/${id}`);
    await send(resumed, id, "continue");
    assert(f.requests.at(-1)!.messages.some((m: Message) => m.content === "keep this original"));
    assert.deepEqual(resumed.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust fails a compaction whose summary calls a tool like Node", async () => {
  const f = await fixture({
    respond(req, res) {
      if (!summaryRequest(req)) return reply(res, "answer");
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, {
        tool_calls: [
          { index: 0, id: "c", type: "function", function: { name: "Read", arguments: "{}" } },
        ],
      });
      end(res, "tool_calls");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "keep this original");
    const after = h.messages.length;
    await h.command(h.envelope("compact", id));
    const frame = await terminal(h, id, "error", after);
    const error = frame.params.frame.payload.deltas.find(
      (d: Message) => d.patch?.control?.phase === "error",
    ).patch.control.lastError;
    assert.equal(error.message, "Tool use is not allowed during compaction");
    const markers = (await h.rows(id)).rows.filter(
      (r) => r.kind === "timelineMarker" && r.marker.type === "compact",
    );
    assert.equal((markers.at(-1) as Message).marker.status, "failed");
  } finally {
    await f.close();
  }
});

test("Rust held queue validates the confirmed set, keeps or clears atomically and records discarded ACKs", async () => {
  const f = await fixture({
    async respond(req, res) {
      if (req.messages.at(-1).content === "hold") await delay(200);
      reply(res, "answer");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "hold" }));
    const queued = h.envelope("sendText", id, { text: "queued" });
    const queuedAck = await h.command(queued);
    // 与 Node 一致：ACK inputId 是 commandId，队列项 id 为 queue_<commandId>。
    assert.equal((queuedAck.result as Message).inputId, queued.commandId);
    const queueId = `queue_${queued.commandId}`;
    await h.command(h.envelope("stop", id));
    await terminal(h, id, "completedInterrupted", after);
    const reject = await h.command(h.envelope("sendText", id, { text: "new" }));
    assert.equal(reject.reasonCode, "heldQueueDispositionRequired");
    const stale = await h.command(
      h.envelope("sendText", id, {
        text: "new",
        heldQueueDisposition: "clearQueueAndSend",
        expectedHeldQueueItemIds: ["outdated"],
      }),
    );
    assert.equal(stale.reasonCode, "guard.heldQueueConfirmationStale");
    let start = h.messages.length;
    await h.command(
      h.envelope("sendText", id, {
        text: "kept",
        heldQueueDisposition: "keepQueueAndSend",
        expectedHeldQueueItemIds: [queueId],
      }),
    );
    await h.completed(id, start);
    assert.equal(
      (await h.command(h.envelope("sendText", id, { text: "still held" }))).reasonCode,
      "heldQueueDispositionRequired",
    );
    start = h.messages.length;
    await h.command(
      h.envelope("sendText", id, {
        text: "cleared",
        heldQueueDisposition: "clearQueueAndSend",
        expectedHeldQueueItemIds: [queueId],
      }),
    );
    await h.completed(id, start);
    assert.equal((await h.command(queued)).status, "failed");
    assert(!(await h.rows(id)).rows.some((r) => r.kind === "userInput" && r.text === "queued"));
    await h.close();
    const resumed = f.start();
    // 重启后回执从 session_input 终态行反查（Node `lookupExact`）：清除的排队项为 cancelled。
    assert.equal((await resumed.command(queued)).reasonCode, "fault.command.inputCancelled");
    assert.deepEqual([...h.schemaErrors, ...resumed.schemaErrors], []);
  } finally {
    await f.close();
  }
});

test("Rust sendQueuedNow preempts the active request and retains original queue provenance", async () => {
  const f = await fixture({
    async respond(req, res) {
      if (req.messages.at(-1).content === "slow") await delay(250);
      reply(res, "answer");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "slow" }));
    const a = h.envelope("sendText", id, { text: "first queued" });
    const b = h.envelope("sendText", id, { text: "prioritized" });
    await h.command(a);
    await h.command(b);
    const promote = h.envelope("sendQueuedNow", id, {
      queueItemId: `queue_${b.commandId}`,
    });
    assert.equal((await h.command(promote)).status, "accepted");
    assert.equal((await h.command(promote)).status, "duplicate");
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: Message) =>
          d.row?.kind === "turnHeader" &&
          d.row?.sourceCommandId === a.commandId &&
          d.row?.state === "completedSuccess",
      ),
    );
    const users = (await h.rows(id)).rows.filter((r) => r.kind === "userInput");
    assert.deepEqual(
      users.map((r) => r.text),
      ["slow", "prioritized", "first queued"],
    );
    assert.equal(users[1]!.sourceCommandId, b.commandId);
    assert.equal(users[1]!.clientId, b.clientId);
    const missing = await h.command(h.envelope("sendQueuedNow", id, { queueItemId: "missing" }));
    assert.equal(missing.status, "noop");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
