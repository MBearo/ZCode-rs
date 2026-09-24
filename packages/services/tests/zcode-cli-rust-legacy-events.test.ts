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
/** `session/event` params after `after`, schema-checked (D16: turn.started only without Node's extra key). */
function events(h: Harness, after: number) {
  return h.messages
    .slice(after)
    .filter((m) => m.method === "session/event")
    .map((m) => {
      const event = m.params as Message;
      if (event.type === "turn.started") {
        assert.equal(zcodeSessionEventSchema.safeParse(event).success, false);
        const { executionStartedAt, ...payload } = event.payload;
        assert.equal(typeof executionStartedAt, "number");
        zcodeSessionEventSchema.parse({ ...event, payload });
      } else {
        zcodeSessionEventSchema.parse(event);
      }
      return event;
    });
}
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
    const types = sent.map((e) => e.type);
    assert.equal(types[0], "session.titleUpdated");
    assert.equal(types[1], "turn.started");
    assert.deepEqual(types.slice(-2), ["session.updated", "turn.completed"]);
    assert.ok(types.slice(2, -2).every((t) => t === "model.streaming"));
    assert.deepEqual(sent[0]!.payload, { previousTitle: "", source: "first_input", title: "hello" });
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
