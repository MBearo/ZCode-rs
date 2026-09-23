import assert from "node:assert/strict";
import test from "node:test";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fixture, event, end, type Harness } from "./rust-agent-fixture.js";

type Message = Record<string, any>;
const summaryRequest = (req: Message) =>
  req.messages[0].content.startsWith("Summarize the earlier coding conversation");
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
    assert.equal(f.requests[1]!.tools, undefined);
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
    assert(
      request.messages.some(
        (m: Message) => m.role === "user" && m.content.includes("COMPACT_SUMMARY"),
      ),
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

test("Rust automatic and reactive compaction preserve the current input and complete after commit", async () => {
  for (const reactive of [false, true]) {
    let rejected = false;
    const f = await fixture({
      config: reactive
        ? {}
        : // Agent/SendMessage 定义也计入上下文；首轮需容纳完整工具，长回复仍须触发压缩。
          { contextWindow: 18000, maxOutputTokens: 1000, contextBufferTokens: 2000 },
      respond(req, res, attempt) {
        if (summaryRequest(req)) return reply(res, "small durable summary");
        if (reactive && attempt === 2 && !rejected) {
          rejected = true;
          res.writeHead(400, { "content-type": "application/json" });
          res.end('{"error":{"code":"context_length_exceeded"}}');
          return;
        }
        reply(res, attempt === 1 ? "history".repeat(5000) : "continued");
      },
    });
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await send(h, id, "first");
      await send(h, id, "current input");
      assert(f.requests.some(summaryRequest));
      assert.equal(f.requests.at(-1)!.messages.at(-1).content, "current input");
      assert(!JSON.stringify(f.requests.at(-1)).includes("historyhistory"));
      assert.equal(f.requests.at(-1)!.max_tokens, reactive ? 32000 : 1000);
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
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
    const queueId = (queuedAck.result as Message).inputId;
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
    assert.equal((await resumed.command(queued)).reasonCode, "guard.queueDeleted");
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
    const queued = await h.command(b);
    const promote = h.envelope("sendQueuedNow", id, {
      queueItemId: (queued.result as Message).inputId,
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
