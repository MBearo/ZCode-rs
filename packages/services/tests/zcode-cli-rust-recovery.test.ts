import assert from "node:assert/strict";
import test from "node:test";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { fixture, binary } from "./zcode-cli-rust-fixture.js";
import {
  TopicWireFrameAssembler,
  conversationTopicFrameSchema,
} from "@zcode/shared/zcode-protocol-v4";
import { z } from "zod";

test("Rust queues execute FIFO, require CAS, and keep drained/create ACKs on restart", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const creation = h.envelope("createSession", null, { workspaceId: f.cwd });
    const created = (await h.command(creation)).result;
    assert(created?.type === "createSession");
    const id = created.sessionId;
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "hello" }));
    const second = h.envelope("sendText", id, { text: "second" });
    const third = h.envelope("sendText", id, { text: "third" });
    for (const command of [second, third]) {
      const result = (await h.command(command)).result;
      assert(result?.type === "inputAccepted");
      assert.equal(result.delivery, "queue");
    }
    await assert.rejects(
      h.command(h.envelope("setAutoDrain", id, { autoDrain: true })),
      /baseRevision/,
    );
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: any) =>
          d.row?.sourceCommandId === third.commandId && d.row?.state === "completedSuccess",
      ),
    );
    assert.deepEqual(
      f.requests.map((r) => r.messages.findLast((m: any) => m.role === "user").content),
      ["hello", "second", "third"],
    );
    await h.close();
    const recovered = f.start();
    assert.equal((await recovered.command(second)).status, "duplicate");
    assert.deepEqual((await recovered.command(creation)).result, created);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust owner lock prevents a second process recovering active work; EOF closes pending tools", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "slow-shell" }));
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some((d: any) => d.row?.toolName === "Bash"),
    );
    const other = spawn(
      binary,
      ["app-server", "--stdio", "--cwd", f.cwd, "--data-dir", f.dataDir],
      { env: { ...process.env, ZCODE_WORKSPACE_IDENTITY: "" } },
    );
    const exit = once(other, "close");
    let error = "";
    other.stdout.resume();
    other.stderr.on("data", (chunk) => {
      error += chunk;
    });
    assert.deepEqual(await exit, [1, null]);
    assert.match(error, /already owned/);
    await h.close();
    const recovered = f.start();
    await recovered.subscribe(`conversation/${id}`);
    const rows = await recovered.rows(id);
    assert.equal(rows.rows.find((r) => r.kind === "toolCall")?.status, "cancelled");
    await recovered.command(recovered.envelope("sendText", id, { text: "hello" }));
    await recovered.completed(id);
    const messages = f.requests.at(-1)!.messages;
    const pending = messages.findIndex((m: any) => m.tool_calls);
    assert.equal(messages[pending + 1].role, "tool");
    assert.match(messages[pending + 1].content, /Interrupted/);
    assert.deepEqual(recovered.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust backpressure and large history recover through the real App frame assembler", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    for (let i = 0; i < 7; i++) {
      const after = h.messages.length;
      await h.command(h.envelope("sendText", id, { text: "large" }));
      await h.completed(id, after);
    }
    const sub = await h.subscribe(`conversation/${id}`, "mobile", "web-remote-replayable");
    const page = await h.rows(id);
    assert.equal(page.hasMore, true);
    assert(JSON.stringify(page).length < 950_000);
    await h.wait(
      (m) =>
        m.params?.subscriptionId === sub.ack.subscriptionId &&
        m.params.fragmentIndex === m.params.fragmentCount - 1,
    );
    const assembler = new TopicWireFrameAssembler(conversationTopicFrameSchema);
    const wires = h.messages
      .filter((m) => m.params?.subscriptionId === sub.ack.subscriptionId)
      .map((m) => m.params);
    assert(wires.length > 1);
    assert(wires.every((w) => w.kind === "fragment"));
    const assembled = wires.flatMap((wire) => assembler.accept(wire));
    assert.equal(assembled.length, 1);
    assert.equal(assembled[0]!.kind, "complete");
    await h.client.request(
      "v4/connection/flow",
      { connectionId: "mobile", state: "saturated" },
      z.object({}).strict(),
    );
    const after = h.messages.length;
    await h.command(h.envelope("renameSession", id, { title: "renamed during backpressure" }));
    assert(
      !h.messages.slice(after).some((m) => m.params?.subscriptionId === sub.ack.subscriptionId),
    );
    await h.client.request(
      "v4/connection/flow",
      { connectionId: "mobile", state: "drained" },
      z.object({}).strict(),
    );
    await h.wait(
      (m) =>
        m.params?.subscriptionId === sub.ack.subscriptionId &&
        m.params.fragmentIndex === m.params.fragmentCount - 1,
      after,
    );
    const recovery = h.messages
      .slice(after)
      .filter((m) => m.params?.subscriptionId === sub.ack.subscriptionId)
      .flatMap((m) => assembler.accept(m.params));
    assert.equal(recovery[0]?.kind, "complete");
    if (recovery[0]?.kind === "complete" && recovery[0].frame.payload.kind === "snapshot")
      assert.equal(recovery[0].frame.payload.snapshot.meta.title, "renamed during backpressure");
    else assert.fail("Missing authoritative snapshot after flow drained");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
