import assert from "node:assert/strict";
import test from "node:test";
import { setTimeout as delay } from "node:timers/promises";
import type { ServerResponse } from "node:http";
import {
  TopicWireFrameAssembler,
  applyConversationDeltas,
  conversationTopicFrameSchema,
  v4ConversationResyncResultSchema,
  type ConversationSnapshot,
  type ConversationTopicFrame,
} from "@zcode/shared/zcode-protocol-v4";
import { z } from "zod";
import { fixture, event, end, type Harness } from "./zcode-cli-rust-fixture.js";

type Message = Record<string, any>;
type Delivered = { frame: ConversationTopicFrame; kind: string | undefined };

/** 按订阅组装逻辑帧（含分片），保留 deliveryKind。 */
function delivered(h: Harness, subscriptionId: string): Delivered[] {
  const assembler = new TopicWireFrameAssembler(conversationTopicFrameSchema);
  return h.messages
    .filter((m) => m.params?.subscriptionId === subscriptionId)
    .flatMap((m) =>
      assembler
        .accept(m.params)
        .flatMap((r) =>
          r.kind === "complete" ? [{ frame: r.frame, kind: m.params.deliveryKind }] : [],
        ),
    );
}

/** 与客户端 store 相同：快照整体替换，增量须从当前水位接续。 */
function project(frames: Delivered[], base?: ConversationSnapshot): ConversationSnapshot {
  let state = base;
  for (const { frame } of frames) {
    if (frame.payload.kind === "snapshot") {
      state = frame.payload.snapshot;
      continue;
    }
    assert(state, "deltas need an applied base");
    assert.equal(frame.fromSeq, state.seq, "frames continue the watermark");
    state = { ...applyConversationDeltas(state, frame.payload.deltas), seq: frame.toSeq };
  }
  assert(state);
  return state;
}

const visible = (s: ConversationSnapshot) => ({
  rows: s.rows.window.map((r: Message) => ({ kind: r.kind, text: r.text })),
  phase: s.control.phase,
  title: s.meta.title,
});

function stream(res: ServerResponse, chunks: string[]) {
  res.writeHead(200, { "content-type": "text/event-stream" });
  for (const content of chunks) event(res, { content });
  end(res, "stop");
}

async function flow(h: Harness, connectionId: string, state: string) {
  await h.client.request("v4/connection/flow", { connectionId, state }, z.object({}).strict());
}

/** 无 conversation 订阅时，经 sessions-index 等待回合结束。 */
async function settled(h: Harness, id: string, after: number) {
  await h.wait(
    (m) =>
      m.params?.topic === `sessions-index/${h.workspace}` &&
      m.params.frame?.payload?.deltas?.some(
        (d: Message) => d.session?.sessionId === id && d.session.phase === "completedSuccess",
      ),
    after,
  );
}

test("Rust desktop and mobile profiles reach the same state with coalesced frames", async () => {
  const chunks = Array.from({ length: 200 }, (_, i) => `c${i} `);
  const f = await fixture({ respond: (_req, res) => stream(res, chunks) });
  try {
    const h = f.start();
    const id = await h.create();
    const desktop = await h.subscribe(`conversation/${id}`);
    const mobile = await h.subscribe(`conversation/${id}`, "mobile", "web-remote-replayable");
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "stream" }));
    await h.completed(id, after);
    await h.wait(
      (m) =>
        m.params?.subscriptionId === mobile.ack.subscriptionId &&
        m.params.frame?.payload?.deltas?.some(
          (d: Message) => d.patch?.control?.phase === "completedSuccess",
        ),
      after,
    );
    const desktopFrames = delivered(h, desktop.ack.subscriptionId);
    const mobileFrames = delivered(h, mobile.ack.subscriptionId);
    assert.equal(desktopFrames[0]?.kind, "initial");
    const a = project(desktopFrames);
    const b = project(mobileFrames);
    assert.deepEqual(visible(a), visible(b));
    assert(visible(a).rows.some((r) => r.text === chunks.join("")));
    // 流式文本块在刷新窗口内合并，不再逐块成帧。
    assert(desktopFrames.length < chunks.length, `${desktopFrames.length} desktop frames`);
    assert(mobileFrames.length < chunks.length, `${mobileFrames.length} mobile frames`);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust resumes a mobile subscription from its base even with no subscriber in between", async () => {
  const f = await fixture({ respond: (_req, res) => stream(res, ["resumed ", "reply"]) });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`sessions-index/${h.workspace}`);
    const first = await h.subscribe(`conversation/${id}`, "mobile", "web-remote-replayable");
    await h.wait((m) => m.params?.subscriptionId === first.ack.subscriptionId);
    const [initial] = delivered(h, first.ack.subscriptionId);
    assert(initial?.frame.payload.kind === "snapshot");
    const base = initial.frame.payload.snapshot;
    // 手机断开：会话此后无人订阅，但 runtime 仍保留增量日志。
    await flow(h, "mobile", "closed");
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "while away" }));
    await settled(h, id, after);
    // 等回合后的标题等发布落定，后续比较才有确定的水位。
    await delay(400);

    const resumed = await h.client.request(
      "v4/conversation/subscribe",
      {
        topic: `conversation/${id}`,
        connectionId: "mobile",
        clientMode: "web-remote-replayable",
        base: { logEpoch: base.logEpoch, seq: base.seq },
      },
      z.object({ ack: z.object({ subscriptionId: z.string(), mode: z.string() }).passthrough() }),
    );
    assert.equal(resumed.ack.mode, "resume");
    await h.wait((m) => m.params?.subscriptionId === resumed.ack.subscriptionId);
    const frames = delivered(h, resumed.ack.subscriptionId);
    assert.equal(frames[0]?.kind, "initial");
    assert.equal(frames[0]?.frame.fromSeq, base.seq);
    const state = project(frames, base);
    const fresh = await h.subscribe(`conversation/${id}`, "check");
    await h.wait((m) => m.params?.subscriptionId === fresh.ack.subscriptionId);
    assert.deepEqual(visible(state), visible(project(delivered(h, fresh.ack.subscriptionId))));

    // 已对齐：resume 且不发初始帧；epoch 不同：快照。
    const aligned = await h.client.request(
      "v4/conversation/subscribe",
      {
        topic: `conversation/${id}`,
        connectionId: "mobile",
        clientMode: "web-remote-replayable",
        base: { logEpoch: base.logEpoch, seq: frames.at(-1)!.frame.toSeq },
      },
      z.object({ ack: z.object({ subscriptionId: z.string(), mode: z.string() }).passthrough() }),
    );
    assert.equal(aligned.ack.mode, "resume");
    await h.rows(id);
    assert.equal(delivered(h, aligned.ack.subscriptionId).length, 0);
    const stale = await h.client.request(
      "v4/conversation/subscribe",
      {
        topic: `conversation/${id}`,
        connectionId: "mobile",
        clientMode: "web-remote-replayable",
        base: { logEpoch: "stale-epoch", seq: 0 },
      },
      z.object({ ack: z.object({ subscriptionId: z.string(), mode: z.string() }).passthrough() }),
    );
    assert.equal(stale.ack.mode, "snapshot");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust resync resumes from the client base, closes aligned flights and honours forceSnapshot", async () => {
  const f = await fixture({ respond: (_req, res) => stream(res, ["one ", "two"]) });
  try {
    const h = f.start();
    const id = await h.create();
    const sub = await h.subscribe(`conversation/${id}`, "mobile", "web-remote-replayable");
    await h.wait((m) => m.params?.subscriptionId === sub.ack.subscriptionId);
    const [initial] = delivered(h, sub.ack.subscriptionId);
    assert(initial?.frame.payload.kind === "snapshot");
    const base = initial.frame.payload.snapshot;
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "go" }));
    await h.completed(id, after);
    await delay(400);
    const current = delivered(h, sub.ack.subscriptionId).at(-1)!.frame.toSeq;
    const resync = async (params: Message) => {
      const before = delivered(h, sub.ack.subscriptionId).length;
      const result = await h.client.request(
        "v4/conversation/resync",
        {
          topic: `conversation/${id}`,
          subscriptionId: sub.ack.subscriptionId,
          connectionId: "mobile",
          ...params,
        },
        v4ConversationResyncResultSchema,
      );
      await h.rows(id);
      return { mode: result.ack.mode, frames: delivered(h, sub.ack.subscriptionId).slice(before) };
    };
    const behind = await resync({ base: { logEpoch: base.logEpoch, seq: base.seq } });
    assert.equal(behind.mode, "resume");
    assert.equal(behind.frames[0]?.kind, "recovery");
    assert.equal(behind.frames[0]?.frame.fromSeq, base.seq);
    assert.equal(behind.frames[0]?.frame.toSeq, current);
    const aligned = await resync({ base: { logEpoch: base.logEpoch, seq: current } });
    assert.equal(aligned.mode, "resume");
    assert.deepEqual(
      [aligned.frames[0]?.frame.fromSeq, aligned.frames[0]?.frame.toSeq],
      [current, current],
    );
    const forced = await resync({
      base: { logEpoch: base.logEpoch, seq: current },
      forceSnapshot: true,
    });
    assert.equal(forced.mode, "snapshot");
    assert.equal(forced.frames[0]?.frame.payload.kind, "snapshot");
    assert.equal(forced.frames[0]?.kind, "recovery");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust recovers a saturated subscriber by snapshot once its buffer overflows", async () => {
  // 每轮回复 200 KB：桌面窗口内远低于 1 MiB，暂停的手机缓冲在第 6 轮前溢出。
  const f = await fixture({ respond: (_req, res) => stream(res, ["z".repeat(200_000)]) });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const mobile = await h.subscribe(`conversation/${id}`, "mobile", "web-remote-replayable");
    await h.wait((m) => m.params?.subscriptionId === mobile.ack.subscriptionId);
    await flow(h, "mobile", "saturated");
    const after = h.messages.length;
    for (let i = 0; i < 6; i++) {
      const turn = h.messages.length;
      await h.command(h.envelope("sendText", id, { text: `turn ${i}` }));
      await h.completed(id, turn);
    }
    const mine = (m: Message) => m.params?.subscriptionId === mobile.ack.subscriptionId;
    assert(!h.messages.slice(after).some(mine), "nothing is sent while saturated");
    await flow(h, "mobile", "drained");
    await h.wait(
      (m) =>
        mine(m) &&
        (m.params.kind === "complete" || m.params.fragmentIndex === m.params.fragmentCount - 1),
      after,
    );
    const recovered = delivered(h, mobile.ack.subscriptionId).slice(1);
    assert.equal(recovered[0]?.frame.payload.kind, "snapshot");
    assert.equal(recovered[0]?.kind, "online");
    assert.equal(project(recovered).control.phase, "completedSuccess");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust resumes the sessions index from a base", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const topic = `sessions-index/${h.workspace}`;
    const first = await h.subscribe(topic, "mobile", "web-remote-replayable");
    const initial = await h.wait((m) => m.params?.subscriptionId === first.ack.subscriptionId);
    const seq = initial.params.frame.toSeq as number;
    await flow(h, "mobile", "closed");
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const after = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "hello" }));
    await h.completed(id, after);
    const resumed = await h.client.request(
      "v4/conversation/subscribe",
      {
        topic,
        connectionId: "mobile",
        clientMode: "web-remote-replayable",
        base: { logEpoch: first.ack.logEpoch, seq },
      },
      z.object({ ack: z.object({ subscriptionId: z.string(), mode: z.string() }).passthrough() }),
    );
    assert.equal(resumed.ack.mode, "resume");
    const frame = await h.wait((m) => m.params?.subscriptionId === resumed.ack.subscriptionId);
    assert.equal(frame.params.deliveryKind, "initial");
    assert.equal(frame.params.frame.fromSeq, seq);
    assert(
      frame.params.frame.payload.deltas.some(
        (d: Message) => d.op === "session.upserted" && d.session.sessionId === id,
      ),
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
