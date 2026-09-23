import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { join } from "node:path";
import { setTimeout as delay } from "node:timers/promises";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { commandsQueryResultSchema } from "@zcode/shared/zcode-protocol-v4";

test(
  "Rust stop reaps an yolo running shell and accepts the next turn",
  { skip: process.platform === "win32" },
  async () => {
    const f = await fixture();
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "slow-shell" }));
      let shellPid: number | undefined;
      for (let attempts = 0; attempts < 100 && !shellPid; attempts++) {
        shellPid = await readFile(join(f.cwd, "shell.pid"), "utf8")
          .then(Number)
          .catch(() => undefined);
        if (!shellPid) await delay(10);
      }
      assert(shellPid);
      process.kill(shellPid, 0);
      await h.command(h.envelope("stop", id));
      await h.wait((m) =>
        m.params?.frame?.payload?.deltas?.some(
          (d: any) => d.patch?.control?.phase === "completedInterrupted",
        ),
      );
      assert.throws(() => process.kill(shellPid!, 0), { code: "ESRCH" });
      const after = h.messages.length;
      await h.command(h.envelope("sendText", id, { text: "hello" }));
      await h.completed(id, after);
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  },
);

test("Rust stdio: current App schemas, streaming, idempotency, resume and workspace isolation", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    const result = await h.subscribe(`conversation/${id}`);
    const initial = await h.wait((m) => m.params?.subscriptionId === result.ack.subscriptionId);
    const initialIndex = h.messages.indexOf(initial);
    assert(
      h.messages
        .slice(0, initialIndex)
        .some((m) => m.result?.ack?.subscriptionId === result.ack.subscriptionId),
    );
    const command = h.envelope("sendText", id, { text: "hello" });
    assert.equal((await h.command(command)).status, "accepted");
    assert.equal((await h.command(command)).status, "duplicate");
    await h.completed(id);
    assert.equal(f.requests.length, 1);
    const rows = await h.rows(id);
    assert.equal(rows.rows.find((r) => r.kind === "assistantText")?.text, "你好 Rust");
    assert(
      h.messages.some((m) =>
        m.params?.frame?.payload?.deltas?.some((d: any) => d.op === "row.delta"),
      ),
    );
    assert.equal(
      (
        await h.command({
          ...h.envelope("renameSession", id, { title: "stale" }),
          baseRevision: 999,
        })
      ).status,
      "stale",
    );
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
    const recovered = f.start();
    await recovered.subscribe(`conversation/${id}`);
    assert.equal((await recovered.rows(id)).rows.length, rows.rows.length);
    assert.equal((await recovered.command(command)).status, "duplicate");
    const other = f.start("fixture:other-workspace");
    await assert.rejects(other.subscribe(`conversation/${id}`), /Session unavailable/);
    assert.deepEqual(recovered.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust yolo executes writes without approvals and rejects other modes", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    for (const mode of ["build", "edit", "plan"]) {
      await assert.rejects(
        h.command(h.envelope("sendText", id, { text: "write", mode })),
        /Unsupported/,
      );
    }
    const command = h.envelope("sendText", id, { text: "write", mode: "yolo" });
    assert.equal((await h.command(command)).status, "accepted");
    assert.equal((await h.command(command)).status, "duplicate");
    await h.completed(id);
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "written by Rust");
    assert(
      !h.messages.some((m) =>
        m.params?.frame?.payload?.deltas?.some((d: any) => d.patch?.pendingInteractions?.length),
      ),
    );
    assert(
      h.messages.some((m) =>
        m.params?.frame?.payload?.deltas?.some((d: any) => d.patch?.config?.mode === "yolo"),
      ),
    );
    assert.equal(f.requests.length, 2);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust cancellation remains responsive while awaiting provider and preserves queued input disposition", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "slow" }));
    const queued = h.envelope("sendText", id, { text: "queued" });
    assert.equal((await h.command(queued)).result?.type, "inputAccepted");
    assert.equal((await h.command(h.envelope("stop", id))).status, "accepted");
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: any) => d.patch?.control?.phase === "completedInterrupted",
      ),
    );
    await h.close();
    const recovered = f.start();
    const query = await recovered.client.request(
      "v4/commands/query",
      { commands: [{ sessionId: id, commandId: queued.commandId }] },
      commandsQueryResultSchema,
    );
    assert.notEqual(query.results[0]!.result, "unknown");
    assert.equal((query.results[0]!.result as any).reasonCode, "fault.input.discardedOnRestart");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust runs yolo shell and keeps desktop/mobile subscriptions separate", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const id = await h.create();
    const desktop = await h.subscribe(`conversation/${id}`);
    const mobile = await h.subscribe(
      `conversation/${id}`,
      "fixture-mobile",
      "web-remote-replayable",
    );
    assert.notEqual(desktop.ack.subscriptionId, mobile.ack.subscriptionId);
    await h.command(h.envelope("sendText", id, { text: "shell" }));
    await h.completed(id);
    assert.match(f.requests[1]!.messages.at(-1).content, /core-shell/);
    for (const sub of [desktop, mobile]) {
      const frames = h.messages
        .filter((m) => m.params?.subscriptionId === sub.ack.subscriptionId)
        .map((m) => m.params.frame);
      let seq = frames[0].toSeq;
      for (const frame of frames.slice(1)) {
        assert.equal(frame.fromSeq, seq);
        seq = frame.toSeq;
      }
    }
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
