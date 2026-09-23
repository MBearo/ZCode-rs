import assert from "node:assert/strict";
import test from "node:test";
import { fixture, event, end, type Harness } from "./rust-agent-fixture.js";
import { importShared, sharedSnapshot, ref, markdown } from "./rust-agent-shared-fixture.js";

async function blocked() {
  const started = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();
  const f = await fixture({
    respond: async (_req, res, attempt) => {
      if (attempt === 1) {
        started.resolve();
        await release.promise;
      }
      if (res.destroyed) return;
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: "done" });
      end(res, "stop");
    },
  });
  const h = f.start();
  await importShared(h, f.cwd);
  await sharedSnapshot(h);
  const first = h.envelope("sendText", "shared-A", { text: "first" });
  await h.command(first);
  await started.promise;
  return { f, h, release, first };
}
async function finished(h: Harness, commandId: string) {
  await h.wait((m) =>
    m.params?.frame?.payload?.deltas?.some(
      (d: any) =>
        d.row?.sourceCommandId === commandId &&
        d.row?.kind === "turnHeader" &&
        d.row.state === "completedSuccess",
    ),
  );
}
test("Shared context-only input follows the existing Composer empty-text contract", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    await importShared(h, f.cwd);
    await sharedSnapshot(h);
    await assert.rejects(
      h.command(h.envelope("sendText", "shared-A", { text: "" })),
      /Invalid input text/,
    );
    const command = h.envelope("sendText", "shared-A", { text: "", context_refs: ref() });
    assert.equal((await h.command(command)).status, "accepted");
    await finished(h, command.commandId);
    assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "attached");
    assert.equal(f.requests[0]!.messages.filter((m: any) => m.content === markdown).length, 1);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    await f.close();
  }
});

test("Shared startNow attaches after cancellation and consumes the reservation exactly once", async () => {
  const { f, h, release, first } = await blocked();
  try {
    const command = h.envelope("sendText", "shared-A", {
      text: "preempt",
      context_refs: ref(),
      requestedDelivery: "startNow",
    });
    assert.equal((await h.command(command)).status, "accepted");
    await finished(h, command.commandId);
    const snapshot = await sharedSnapshot(h);
    assert.equal(snapshot.sharedContextImport.status, "attached");
    assert.equal(
      snapshot.rows.window.find(
        (r: any) => r.kind === "turnHeader" && r.sourceCommandId === first.commandId,
      ).state,
      "completedInterrupted",
    );
    assert.equal(f.requests[1]!.messages.filter((m: any) => m.content === markdown).length, 1);
    release.resolve();
    assert.equal((await h.command(command)).status, "duplicate");
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    release.resolve();
    await f.close();
  }
});

test("Shared queue reservation survives edit/reorder, is released by deletion, and attaches at queued-now promotion", async () => {
  const { f, h, release } = await blocked();
  try {
    const queued = h.envelope("sendText", "shared-A", { text: "queued", context_refs: ref() });
    const ack = await h.command(queued);
    const queueId = (ack.result as any).inputId;
    let s = await sharedSnapshot(h);
    assert.equal(s.sharedContextImport.status, "reserved");
    assert.deepEqual(s.queue.items[0].sharedContextRefs, ref());
    await assert.rejects(
      h.command(h.envelope("sendText", "shared-A", { text: "compete", context_refs: ref() })),
      /NotAttachable/,
    );
    assert.equal(
      (await h.command(h.envelope("discardSharedContext", "shared-A", { contextId: "context-A" })))
        .status,
      "rejected",
    );
    assert.equal(
      (
        await h.command({
          ...h.envelope("deleteQueueItem", "shared-A", { queueItemId: queueId }),
          baseRevision: s.revision,
        })
      ).status,
      "accepted",
    );
    assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "pending");
    assert.equal((await h.command(queued)).status, "failed");
    const next = h.envelope("sendText", "shared-A", { text: "again", context_refs: ref() });
    const nextId = (await h.command(next)).result as any;
    await h.command(h.envelope("sendText", "shared-A", { text: "unrelated" }));
    s = await sharedSnapshot(h);
    await h.command({
      ...h.envelope("editQueueItem", "shared-A", {
        queueItemId: nextId.inputId,
        newText: "edited",
      }),
      baseRevision: s.revision,
    });
    s = await sharedSnapshot(h);
    await h.command({
      ...h.envelope("reorderQueueItem", "shared-A", {
        queueItemId: nextId.inputId,
        beforeQueueItemId: null,
      }),
      baseRevision: s.revision,
    });
    await h.command(h.envelope("sendQueuedNow", "shared-A", { queueItemId: nextId.inputId }));
    await finished(h, next.commandId);
    assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "attached");
    const attached = f.requests.find((r) => r.messages.some((m: any) => m.content === markdown))!;
    assert(attached);
    assert.match(JSON.stringify(attached.messages), /edited/);
    release.resolve();
    await h.close();
  } finally {
    release.resolve();
    await f.close();
  }
});

for (const operation of ["restart", "close", "clear"] as const)
  test(`Shared reservation returns to pending after ${operation}`, async () => {
    const { f, h, release } = await blocked();
    try {
      const queued = h.envelope("sendText", "shared-A", { text: "candidate", context_refs: ref() });
      await h.command(queued);
      assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "reserved");
      if (operation === "restart") {
        await h.close();
        release.resolve();
        const cold = f.start();
        assert.equal((await sharedSnapshot(cold)).sharedContextImport.status, "pending");
        assert.equal((await cold.command(queued)).status, "failed");
        await cold.close();
      } else if (operation === "close") {
        await h.command(h.envelope("deleteSession", "shared-A"));
        assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "pending");
        assert.equal((await h.command(queued)).status, "failed");
        await h.close();
      } else {
        await h.command(h.envelope("stop", "shared-A"));
        await h.wait((m) =>
          m.params?.frame?.payload?.deltas?.some(
            (d: any) => d.patch?.control?.phase === "completedInterrupted",
          ),
        );
        const s = await sharedSnapshot(h);
        const command = h.envelope("sendText", "shared-A", {
          text: "clear pending queue",
          heldQueueDisposition: "clearQueueAndSend",
          expectedHeldQueueItemIds: s.queue.items.map((q: any) => q.queueItemId),
        });
        await h.command(command);
        await finished(h, command.commandId);
        assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "pending");
        assert(!JSON.stringify(f.requests).includes("SHARED_CONTEXT_SECRET"));
        await h.close();
      }
    } finally {
      release.resolve();
      await f.close();
    }
  });

test("Shared guide commits hidden context and steering in order at the same run boundary", async () => {
  const { f, h, release, first } = await blocked();
  try {
    await h.command(
      h.envelope("sendText", "shared-A", {
        text: "guided",
        requestedDelivery: "guide",
        context_refs: ref(),
      }),
    );
    assert.equal((await sharedSnapshot(h)).sharedContextImport.status, "reserved");
    release.resolve();
    await finished(h, first.commandId);
    assert.equal(f.requests.length, 2);
    const messages = f.requests[1]!.messages;
    const index = messages.findIndex((m: any) => m.content === markdown);
    assert(index > 0);
    assert.match(messages[index + 1].content, /guided/);
    const s = await sharedSnapshot(h);
    assert.equal(s.sharedContextImport.status, "attached");
    assert.equal(s.rows.window.filter((r: any) => r.kind === "turnHeader").length, 1);
    assert(!JSON.stringify(s.rows).includes("SHARED_CONTEXT_SECRET"));
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    release.resolve();
    await f.close();
  }
});
