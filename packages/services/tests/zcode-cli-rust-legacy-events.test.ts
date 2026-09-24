import assert from "node:assert/strict";
import test from "node:test";
import {
  zcodeSessionEventSchema,
  zcodeSessionSubscribeResultSchema,
  zcodeStateUpdatedNotificationSchema,
} from "@zcode/shared";
import { fixture, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;

function subscribe(h: Harness, sessionId: string, deliveryKind: string, includeSnapshot = false) {
  return h.client.request(
    "session/subscribe",
    { sessionId, deliveryKind, includeSnapshot },
    zcodeSessionSubscribeResultSchema,
  );
}
/** D16: Node's keys outside the strict schema, which make the Host drop the event. */
function nodeExtraKey(event: Message) {
  if (event.type === "turn.started") return "executionStartedAt";
  if (event.type === "tool.updated" && event.payload.kind === "started") return "readOnly";
  if (event.type === "permission.requested" && event.payload.fullAccessSupported) {
    return "fullAccessSupported";
  }
  return undefined;
}
/** `session/event` params after `after`, schema-checked. */
function events(h: Harness, after: number) {
  return h.messages
    .slice(after)
    .filter((m) => m.method === "session/event")
    .map((m) => {
      const event = m.params as Message;
      const extra = nodeExtraKey(event);
      if (extra) {
        assert.equal(zcodeSessionEventSchema.safeParse(event).success, false);
        const { [extra]: value, ...payload } = event.payload;
        assert.notEqual(value, undefined);
        zcodeSessionEventSchema.parse({ ...event, payload });
      } else {
        zcodeSessionEventSchema.parse(event);
      }
      return event;
    });
}
const tool = (sent: Message[], kind: string) =>
  sent.filter((e) => e.type === "tool.updated" && e.payload.kind === kind);
async function stateUpdated(h: Harness, after: number) {
  const m = await h.wait((m) => m.method === "state.updated", after);
  return {
    index: h.messages.indexOf(m),
    params: zcodeStateUpdatedNotificationSchema.parse(m.params),
  };
}
async function turn(h: Harness, id: string, text: string) {
  const before = h.messages.length;
  const command = h.envelope("sendText", id, { text });
  await h.command(command);
  await h.completed(id, before);
  return { before, command };
}

test("Rust legacy stream sends a text turn like Node and state.updated after it", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const subscribed = await subscribe(h, id, "web-remote-replayable", true);
    assert.equal(subscribed.eventSeq, 0);
    assert.deepEqual(subscribed.events, []);
    assert.equal(subscribed.snapshot?.runtime.deliveryKind, "web-remote-replayable");
    assert.equal(subscribed.snapshot?.runtime.stateRevision, 0);
    const { before, command } = await turn(h, id, "hello");
    const done = await stateUpdated(h, before);
    const sent = events(h, before);
    // session.updated 以载荷区分：网络状态带 type，model_complete 不带。
    const types = sent.map((e) =>
      e.type === "session.updated" ? (e.payload.type ?? "model_complete") : e.type,
    );
    assert.deepEqual(types.slice(0, 3), [
      "session.titleUpdated",
      "turn.started",
      "model_request_started",
    ]);
    assert.deepEqual(types.slice(-3), [
      "model_request_completed",
      "model_complete",
      "turn.completed",
    ]);
    assert.ok(types.slice(3, -3).every((t) => t === "model.streaming"));
    assert.deepEqual(sent[0]!.payload, {
      previousTitle: "",
      source: "first_input",
      title: "hello",
    });
    const started = sent[1]!;
    assert.equal(started.payload.input, "hello");
    assert.equal(started.payload.inputId, command.commandId);
    assert.equal(started.payload.turnNumber, 0);
    const text = sent
      .filter((e) => e.type === "model.streaming")
      .map((e) => e.payload.delta)
      .join("");
    assert.equal(text, "你好 Rust");
    const completed = sent.at(-1)!;
    assert.equal(completed.payload.response, "你好 Rust");
    assert.equal(completed.payload.resultType, "success");
    assert.equal(completed.payload.inputId, command.commandId);
    assert.equal(completed.payload.usage.inputTokens, 10);
    assert.equal(completed.payload.usage.modelRequestCount, 1);
    assert.ok(sent.every((e) => e.turnId === started.turnId && e.sessionId === id));
    assert.deepEqual(
      sent.map((e) => e.seq),
      sent.map((_, i) => i + 1),
    );
    assert.equal(new Set(sent.map((e) => e.eventId)).size, sent.length);
    const modelComplete = sent.at(-2)!.payload;
    assert.equal(modelComplete.type, undefined);
    assert.equal(modelComplete.querySource, "main_turn");
    assert.equal(modelComplete.stopReason, "stop");
    // state.updated 在 turn.completed 之后，revision 为 legacy 计数。
    const completedIndex = h.messages.findIndex((m) => m.params?.eventId === completed.eventId);
    assert.ok(done.index > completedIndex);
    assert.equal(done.params.reason, "prompt_completed");
    assert.equal(done.params.revision, 1);
    assert.deepEqual(done.params.workspace, { workspacePath: f.cwd, workspaceKey: f.cwd });
    // 第二轮：turnNumber 递增，不再发标题；seq 接续。
    const second = await turn(h, id, "again");
    const next = events(h, second.before);
    assert.equal(next[0]!.type, "turn.started");
    assert.equal(next[0]!.payload.turnNumber, 1);
    assert.equal(next[0]!.seq, sent.length + 1);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust legacy stream stays silent until subscribed and numbers each delivery kind", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const quiet = await turn(h, id, "hello");
    // 未订阅：没有 session/event，但 state.updated 照常发出。
    assert.equal((await stateUpdated(h, quiet.before)).params.reason, "prompt_completed");
    assert.deepEqual(events(h, quiet.before), []);
    assert.equal((await subscribe(h, id, "desktop-continuous")).eventSeq, 0);
    const desktop = await turn(h, id, "one");
    const continuous = events(h, desktop.before);
    assert.ok(continuous.every((e) => e.deliveryKind === "desktop-continuous"));
    assert.equal((await subscribe(h, id, "web-remote-replayable")).eventSeq, 0);
    const phone = await turn(h, id, "two");
    const replayable = events(h, phone.before);
    assert.equal(replayable[0]!.seq, 1);
    assert.ok(replayable.every((e) => e.deliveryKind === "web-remote-replayable"));
    assert.equal((await subscribe(h, id, "desktop-continuous")).eventSeq, continuous.length);
    const missing = await subscribe(h, "sess_gone", "desktop-continuous").catch((e) => e);
    assert.equal(missing.code, -32004);
    assert.equal(missing.message, "Session is not active: sess_gone");
  } finally {
    await f.close();
  }
});

test("Rust legacy stream reports failed and cancelled turns like Node", async () => {
  const f = await fixture({
    async respond(request, res) {
      const text = request.messages.findLast((m: Message) => m.role === "user")?.content;
      if (text === "slow") {
        await new Promise((resolve) => setTimeout(resolve, 2000));
        if (res.destroyed) return;
      }
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: "invalid fixture request" } }));
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await subscribe(h, id, "web-remote-replayable");
    const failedAt = h.messages.length;
    const command = h.envelope("sendText", id, { text: "fail" });
    await h.command(command);
    const failedUpdate = await stateUpdated(h, failedAt);
    const failure = events(h, failedAt).at(-1)!;
    assert.equal(failure.type, "turn.failed");
    assert.equal(failure.payload.turnPhase, "execution");
    assert.equal(failure.payload.inputId, command.commandId);
    assert.ok(failure.payload.error.message.length > 0);
    assert.equal(failedUpdate.params.reason, "prompt_failed");
    const before = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "slow" }));
    await h.wait((m) => m.params?.type === "turn.started", before);
    await h.command(h.envelope("stop", id));
    const stopped = await stateUpdated(h, before);
    const cancelled = events(h, before).at(-1)!;
    assert.equal(cancelled.type, "turn.completed");
    assert.equal(cancelled.payload.resultType, "cancelled");
    assert.equal(cancelled.payload.response, "");
    assert.equal(stopped.params.reason, "prompt_failed");
  } finally {
    await f.close();
  }
});

test("Rust legacy stream reports a tool call's schedule, start, result and batch", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await subscribe(h, id, "web-remote-replayable");
    const { before } = await turn(h, id, "shell");
    const sent = events(h, before);
    const [scheduled] = tool(sent, "scheduled");
    assert.equal(scheduled?.payload.toolCallId, "call-shell");
    assert.equal(scheduled?.payload.toolName, "Bash");
    assert.equal(typeof scheduled?.payload.input.command, "string");
    assert.deepEqual(scheduled?.payload.schedule.executionOrder, ["call-shell"]);
    const order = sent.map((e) => (e.type === "tool.updated" ? e.payload.kind : e.type));
    const at = (kind: string) => order.indexOf(kind);
    assert.ok(at("session.updated") < at("scheduled"));
    assert.ok(at("scheduled") < at("started") && at("started") < at("result"));
    assert.ok(at("result") < at("batch"));
    assert.equal(tool(sent, "result")[0]?.payload.result.content.includes("core-shell"), true);
    assert.deepEqual(tool(sent, "batch")[0]?.payload, {
      toolCallIds: ["call-shell"],
      successCount: 1,
      errorCount: 0,
      kind: "batch",
    });
    const completed = sent.at(-1)!;
    assert.equal(completed.payload.toolCallCount, 1);
    assert.equal(completed.payload.historyRoundCount, 2);
  } finally {
    await f.close();
  }
});

test("Rust legacy stream sends permission prompts and their answers like Node", async () => {
  const f = await fixture({ permissionMode: "build" });
  try {
    const h = f.start();
    for (const [text, option, decision] of [
      ["write", "allowOnce", "allow"],
      ["deny", "deny", "deny"],
    ] as const) {
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await subscribe(h, id, "web-remote-replayable");
      const before = h.messages.length;
      await h.command(h.envelope("sendText", id, { text }));
      const prompt = await h.permission(id);
      await h.command(
        h.envelope("resolveInteraction", id, {
          interactionId: prompt.interactionId,
          answer: { optionId: option },
        }),
      );
      await stateUpdated(h, before);
      const sent = events(h, before);
      const requested = sent.find((e) => e.type === "permission.requested")!;
      assert.equal(requested.payload.requestId, prompt.interactionId);
      assert.equal(requested.payload.toolName, "Write");
      assert.deepEqual(
        requested.payload.options.map((o: Message) => o.optionId),
        ["allow_once", "allow_project", "deny"],
      );
      const resolved = sent.filter((e) => e.type === "permission.resolved");
      assert.equal(resolved.length, 1);
      assert.equal(resolved[0]!.payload.decision, decision);
      assert.equal(resolved[0]!.payload.requestId, prompt.interactionId);
      const kinds = tool(sent, "result").length + tool(sent, "error").length;
      // 拒绝不再产生工具结果或错误（Node permission_denied 路径）。
      assert.equal(kinds, decision === "allow" ? 1 : 0);
      assert.equal(tool(sent, "started").length, decision === "allow" ? 1 : 0);
    }
  } finally {
    await f.close();
  }
});
