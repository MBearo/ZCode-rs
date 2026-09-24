import assert from "node:assert/strict";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import {
  zcodeSessionCompactResultSchema,
  zcodeSessionEventSchema,
  zcodeSessionGoalResultSchema,
  zcodeSessionSendResultSchema,
  zcodeSessionStateSnapshotSchema,
  zcodeSessionSubscribeResultSchema,
  zcodeStateUpdatedNotificationSchema,
} from "@zcode/shared";
import { end, event, fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
// 有效的 1x1 PNG：提示图片按 Node 规则解码，CRC 错误的图片会降级为占位。
const png =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR4nGP4Xw8AAoABfwxp8mEAAAAASUVORK5CYII=";
const properties = {
  inputFormat: {
    supportsText: true,
    supportsImage: true,
    supportsPdf: true,
    supportsVideo: true,
    supportsAudio: false,
  },
  outputFormat: { supportsText: true },
};

/** A legacy session subscribed to the phone stream, as the Host does. */
async function legacySession(h: Harness) {
  const workspace = { workspacePath: h.workspace, workspaceKey: h.workspace };
  const created = await h.client.request(
    "session/create",
    { workspace },
    zcodeSessionStateSnapshotSchema,
  );
  const id = created.session.sessionId;
  await h.subscribe(`conversation/${id}`);
  await h.client.request(
    "session/subscribe",
    { sessionId: id, deliveryKind: "web-remote-replayable" },
    zcodeSessionSubscribeResultSchema,
  );
  return id;
}
const send = (h: Harness, params: Message) =>
  h.client.request("session/send", params, zcodeSessionSendResultSchema);
const compact = (h: Harness, params: Message) =>
  h.client.request("session/compact", params, zcodeSessionCompactResultSchema);
const goal = (h: Harness, params: Message) =>
  h.client.request("session/goal", params, zcodeSessionGoalResultSchema);
async function rejection(promise: Promise<unknown>) {
  try {
    await promise;
  } catch (error) {
    return error as { code?: number; message: string; data?: any };
  }
  throw new Error("expected rejection");
}
/** `state.updated` reasons and revisions after `after`. */
function states(h: Harness, after: number) {
  return h.messages
    .slice(after)
    .filter((m) => m.method === "state.updated")
    .map((m) => zcodeStateUpdatedNotificationSchema.parse(m.params));
}
const reached = (h: Harness, reason: string, after: number) =>
  h.wait((m) => m.method === "state.updated" && m.params.reason === reason, after);
/** Legacy events of `type` after `after`, strict-checked unless Node adds `executionStartedAt`. */
function legacy(h: Harness, type: string, after: number) {
  return h.messages
    .slice(after)
    .filter((m) => m.method === "session/event" && m.params.type === type)
    .map((m) => {
      if (m.params.payload?.executionStartedAt === undefined)
        zcodeSessionEventSchema.parse(m.params);
      return m.params as Message;
    });
}

test("Rust legacy session/send maps Host attachments and holds the legacy lock", async () => {
  const f = await fixture({ config: { formatProperties: properties } });
  try {
    const h = f.start();
    const id = await legacySession(h);
    const after = h.messages.length;
    const voice = Buffer.from("voice").toString("base64");
    const result = await send(h, {
      sessionId: id,
      inputId: "trace-1",
      content: "看附件",
      attachments: [
        { kind: "image", filename: "dot.png", mimeType: "image/*", dataBase64: png },
        { kind: "file", filename: "notes.md", mimeType: "text/markdown", textContent: "# 笔记" },
        { kind: "file", filename: "big.bin", dataBase64: "aGk=", sizeBytes: 70_000 },
        { kind: "audio", filename: "voice.m4a", mimeType: "audio/mp4", dataBase64: voice },
      ],
    });
    assert.deepEqual(result, { sessionId: id, accepted: true, stateRevision: 1 });
    await reached(h, "prompt_completed", after);
    assert.deepEqual(
      states(h, after).map((s) => [s.reason, s.revision]),
      [
        ["prompt_started", 1],
        ["prompt_completed", 2],
      ],
    );
    assert.deepEqual(states(h, after)[0]!.patch, { status: "running" });
    assert.equal(legacy(h, "turn.started", after)[0]!.payload.inputId, "trace-1");
    assert.equal(legacy(h, "turn.completed", after)[0]!.payload.inputId, "trace-1");
    // Node `buildRuntimeUserEntriesFromTurn`：图片随 user 消息，文本附件是其后的 prompt_attachment 提醒。
    const messages = f.requests.at(-1)!.messages;
    const start = messages.findLastIndex((m: Message) =>
      JSON.stringify(m.content).includes("看附件"),
    );
    const content = JSON.stringify(messages.slice(start));
    for (const part of ["data:image/png;base64,", "notes.md", "# 笔记", "voice.m4a"]) {
      assert.ok(content.includes(part), part);
    }
    assert.ok(!content.includes("big.bin"));
    const rows: Message[] = (await h.rows(id)).rows;
    const input = rows.find((r) => r.kind === "userInput")!;
    assert.deepEqual(
      input.attachments.map((a: Message) => [a.fileName, a.mime]),
      [
        ["dot.png", "image/png"],
        ["notes.md", "text/markdown"],
        ["voice.m4a", "application/octet-stream"],
      ],
    );

    const busyAfter = h.messages.length;
    await send(h, { sessionId: id, content: "slow" });
    const locked = await rejection(send(h, { sessionId: id, content: "again" }));
    assert.deepEqual(
      [locked.code, locked.message],
      [-32010, "A prompt is already running for this session"],
    );
    const compacting = await rejection(compact(h, { sessionId: id }));
    assert.deepEqual(
      [compacting.code, compacting.message],
      [-32010, "Cannot compact while a prompt is running"],
    );
    const managing = await rejection(goal(h, { sessionId: id, action: "show" }));
    assert.deepEqual(
      [managing.code, managing.message],
      [-32010, "Cannot manage goals while a prompt is running"],
    );
    // revision 校验先于锁。
    const stale = await rejection(send(h, { sessionId: id, content: "x", expectedRevision: 0 }));
    assert.equal(stale.code, -32009);
    assert.deepEqual(stale.data, { actualRevision: 3, expectedRevision: 0 });
    const invalid = await rejection(
      send(h, { sessionId: id, content: "x", offPeakRunType: "init" }),
    );
    assert.equal(
      invalid.message,
      "Invalid params — offPeakRunType: offPeakRunType requires offPeakTaskId",
    );
    const missing = await rejection(send(h, { sessionId: "missing", content: "x" }));
    assert.deepEqual([missing.code, missing.message], [-32004, "Session is not active: missing"]);
    await reached(h, "prompt_completed", busyAfter);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust legacy session/send during an unlocked V4 run is accepted and reported at once", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await legacySession(h);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "slow" }));
    const result = await send(h, { sessionId: id, inputId: "guided", content: "补充说明" });
    assert.equal(result.accepted, true);
    // Node 的后台在引导或排队后立即返回：prompt_completed 早于正在进行的 V4 轮结束。
    await reached(h, "prompt_completed", after);
    assert.deepEqual(
      states(h, after).map((s) => s.reason),
      ["prompt_started", "prompt_completed"],
    );
    assert.equal(legacy(h, "turn.completed", after).length, 0);
    for (let i = 0; i < 100 && !JSON.stringify(f.requests).includes("补充说明"); i++) {
      await delay(50);
    }
    assert.ok(JSON.stringify(f.requests).includes("补充说明"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust legacy session/compact starts a manual compact and reports already_running", async () => {
  const f = await fixture({
    async respond(req, res) {
      const summary = req.messages[0].content.startsWith(
        "Summarize the earlier coding conversation",
      );
      if (summary) await delay(500);
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: summary ? "COMPACT_SUMMARY" : "answer" });
      end(res, "stop");
    },
  });
  try {
    const h = f.start();
    const id = await legacySession(h);
    await send(h, { sessionId: id, content: "original question" });
    await reached(h, "prompt_completed", 0);
    const after = h.messages.length;
    const first = await compact(h, {
      sessionId: id,
      inputId: "c-1",
      instructions: "  keep tests  ",
    });
    assert.equal(first.response, "");
    assert.deepEqual(first.compact, { state: "accepted", inputId: "c-1" });
    assert.equal(first.snapshot.session.status, "running");
    const second = await compact(h, { sessionId: id });
    assert.deepEqual(second.compact, { state: "already_running" });
    await reached(h, "session_compacted", after);
    assert.deepEqual(
      states(h, after).map((s) => s.reason),
      ["compact_started", "session_compacted"],
    );
    // Node 的压缩 turn.started 没有 executionStartedAt，strict schema 照常接受。
    const started = legacy(h, "turn.started", after);
    assert.deepEqual(started[0]!.payload, {
      turnNumber: 1,
      input: "/compact keep tests",
      inputId: "c-1",
      inputVisibility: "model-only",
    });
    assert.equal(legacy(h, "turn.completed", after)[0]!.payload.inputId, "c-1");
    assert.ok(JSON.stringify(f.requests.at(-1)!.messages).includes("keep tests"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust legacy session/goal sets, shows, pauses, resumes and clears like Node", async () => {
  const f = await fixture({
    respond(req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      const verify = req.messages.at(-1).content.includes("Verify whether the active session goal");
      event(res, { content: verify ? JSON.stringify({ passed: true, reason: "done" }) : "done" });
      end(res, "stop");
    },
  });
  try {
    const h = f.start();
    const id = await legacySession(h);
    const none = await goal(h, { sessionId: id, action: "show" });
    assert.deepEqual(
      [none.response, none.startedTurn],
      ["No goal is set. Use /goal <objective> to set one.", false],
    );
    assert.equal(none.snapshot.session.target, null);
    let after = h.messages.length;
    const set = await goal(h, {
      sessionId: id,
      action: "set",
      objective: "  实现功能  ",
      inputId: "g-1",
    });
    assert.equal(
      set.response,
      "Goal active\nObjective: 实现功能\nUsage: 0 tokens / none\nTime: 0 seconds",
    );
    assert.equal(set.startedTurn, true);
    assert.equal(set.snapshot.session.status, "running");
    assert.equal(set.snapshot.projection.target?.objective, "实现功能");
    assert.equal(set.snapshot.session.target?.status, "active");
    await reached(h, "goal_continuation_completed", after);
    assert.deepEqual(
      states(h, after).map((s) => s.reason),
      ["goal_set", "goal_continuation_completed"],
    );
    assert.equal(legacy(h, "turn.started", after)[0]!.payload.inputId, "g-1");
    const shown = await goal(h, { sessionId: id, action: "show" });
    assert.match(
      shown.response,
      /^Goal complete\nObjective: 实现功能\nUsage: \d+ tokens \/ none\nTime: \d+ seconds$/,
    );
    assert.equal(shown.snapshot.projection.target?.status, "complete");

    after = h.messages.length;
    const paused = await goal(h, { sessionId: id, action: "pause" });
    assert.deepEqual([paused.response, paused.startedTurn], ["", false]);
    const resumed = await goal(h, { sessionId: id, action: "resume", inputId: "g-2" });
    assert.match(resumed.response, /^Goal resumed\nObjective: 实现功能\n/);
    assert.equal(resumed.startedTurn, true);
    await reached(h, "goal_continuation_completed", after);
    assert.deepEqual(
      states(h, after).map((s) => s.reason),
      ["goal_paused", "goal_resumed", "goal_continuation_completed"],
    );
    const continued = legacy(h, "turn.started", after)[0]!.payload;
    assert.deepEqual([continued.inputId, continued.inputSource], ["g-2", "goal-continuation"]);

    const cleared = await goal(h, { sessionId: id, action: "clear" });
    assert.equal(cleared.response, "Goal cleared.");
    assert.equal(cleared.snapshot.projection.target, null);
    assert.equal((await goal(h, { sessionId: id, action: "clear" })).response, "No goal to clear.");
    assert.equal((await goal(h, { sessionId: id, action: "pause" })).response, "No goal to pause.");
    assert.equal(
      (await goal(h, { sessionId: id, action: "resume" })).response,
      "No goal to resume.",
    );
    assert.equal(
      (await goal(h, { sessionId: id, action: "set", objective: " " })).response,
      "Usage: /goal <objective>",
    );
    assert.equal(
      (await goal(h, { sessionId: id, action: "replace" })).response,
      "Usage: /goal replace <objective>",
    );

    await h.client.request(
      "session/setMode",
      { sessionId: id, mode: "plan" },
      zcodeSessionStateSnapshotSchema,
    );
    const requests = f.requests.length;
    const planned = await goal(h, { sessionId: id, action: "set", objective: "规划目标" });
    assert.equal(
      planned.response,
      "Goal active\nObjective: 规划目标\nUsage: 0 tokens / none\nTime: 0 seconds\n\nPlan mode 下已记录 goal，但不会自动继续。",
    );
    assert.equal(planned.startedTurn, false);
    assert.equal(planned.snapshot.session.target?.status, "active");
    assert.notEqual(planned.snapshot.session.status, "running");
    await delay(100);
    assert.equal(f.requests.length, requests);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
