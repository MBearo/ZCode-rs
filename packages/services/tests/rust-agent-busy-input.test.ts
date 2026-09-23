import assert from "node:assert/strict";
import test from "node:test";
import { readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { fixture, event, end, waitForFile, type Harness } from "./rust-agent-fixture.js";
import { configureRegistry } from "./rust-agent-registry-fixture.js";

type Message = Record<string, any>;
async function snapshot(h: Harness, id: string, connection = "inspect") {
  const after = h.messages.length;
  const sub = await h.subscribe(`conversation/${id}`, connection);
  const frame = await h.wait(
    (m) =>
      m.params?.subscriptionId === sub.ack.subscriptionId &&
      m.params?.frame?.payload?.kind === "snapshot",
    after,
  );
  return frame.params.frame.payload.snapshot;
}
async function finished(h: Harness, commandId: string, after = 0) {
  await h.wait(
    (m) =>
      m.params?.frame?.payload?.deltas?.some(
        (d: Message) =>
          d.row?.kind === "turnHeader" &&
          d.row.sourceCommandId === commandId &&
          d.row.state === "completedSuccess",
      ),
    after,
  );
}
function response(res: Parameters<typeof event>[0], text = "answer") {
  res.writeHead(200, { "content-type": "text/event-stream" });
  event(res, { content: text });
  end(res, "stop");
}

test("Rust guide continues text-only steps in the same turn, bypasses future queue and survives restart", async () => {
  const gate = Promise.withResolvers<void>();
  const started = Promise.withResolvers<void>();
  const f = await fixture({
    async respond(_req, res, n) {
      if (n === 1) {
        started.resolve();
        await gate.promise;
      }
      response(res, `answer-${n}`);
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.subscribe(`conversation/${id}`, "phone", "web-remote-replayable");
    const original = h.envelope("sendText", id, { text: "original" });
    await h.command(original);
    await started.promise;
    const queued = h.envelope("sendText", id, { text: "future", requestedDelivery: "queue" });
    await h.command(queued);
    const guides = ["first guide", "second guide"].map((text) =>
      h.envelope("sendText", id, { text, requestedDelivery: "guide" }),
    );
    for (const guide of guides) {
      const ack = await h.command(guide);
      assert.equal(ack.status, "accepted");
      assert.equal((ack.result as Message).delivery, "queue");
    }
    assert.equal((await h.command(guides[0]!)).status, "duplicate");
    gate.resolve();
    await finished(h, queued.commandId);
    assert.equal(f.requests.length, 4);
    assert.match(f.requests[1]!.messages.at(-1).content, /first guide/);
    assert(!JSON.stringify(f.requests[1]).includes("second guide"));
    assert(!JSON.stringify(f.requests[1]).includes('"future"'));
    assert.match(f.requests[2]!.messages.at(-1).content, /second guide/);
    const rows = (await h.rows(id)).rows;
    const users = rows.filter((r) => r.kind === "userInput");
    assert.deepEqual(
      users.map((r) => r.text),
      ["original", "first guide", "second guide", "future"],
    );
    assert.equal(rows.filter((r) => r.kind === "turnHeader").length, 2);
    for (const [n, guide] of guides.entries()) {
      assert.equal(users[n + 1]!.turnId, users[0]!.turnId);
      assert.equal(users[n + 1]!.guided, true);
      assert.equal(users[n + 1]!.sourceCommandId, guide.commandId);
      assert.equal(users[n + 1]!.clientId, guide.clientId);
    }
    await h.close();
    const resumed = f.start();
    const cold = (await resumed.rows(id)).rows.filter((r) => r.kind === "userInput");
    assert.deepEqual(cold, users);
    assert.equal((await resumed.command(guides[0]!)).status, "duplicate");
    assert.deepEqual([...h.schemaErrors, ...resumed.schemaErrors], []);
  } finally {
    gate.resolve();
    await f.close();
  }
});

test("Rust guide waits for all tool results including failures before continuing", async () => {
  const gate = Promise.withResolvers<void>();
  const started = Promise.withResolvers<void>();
  const f = await fixture({
    async respond(_req, res, n) {
      if (n > 1) {
        response(res);
        return;
      }
      started.resolve();
      await gate.promise;
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, {
        tool_calls: [
          {
            index: 0,
            id: "missing",
            type: "function",
            function: { name: "Read", arguments: JSON.stringify({ file_path: "missing.txt" }) },
          },
          {
            index: 1,
            id: "write",
            type: "function",
            function: {
              name: "Write",
              arguments: JSON.stringify({ file_path: "result.txt", content: "exactly once" }),
            },
          },
        ],
      });
      end(res, "tool_calls");
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const first = h.envelope("sendText", id, { text: "tools" });
    await h.command(first);
    await started.promise;
    const ack = await h.command(
      h.envelope("sendText", id, { text: "after all tools", requestedDelivery: "guide" }),
    );
    assert.equal(ack.status, "accepted");
    gate.resolve();
    await finished(h, first.commandId);
    assert.equal(f.requests.length, 2);
    const tail = f.requests[1]!.messages.slice(-4);
    assert.deepEqual(
      tail.map((m: Message) => m.role),
      ["assistant", "tool", "tool", "user"],
    );
    assert.deepEqual(
      tail.slice(1, 3).map((m: Message) => m.tool_call_id),
      ["missing", "write"],
    );
    assert.match(tail.at(-1).content, /after all tools/);
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "exactly once");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    gate.resolve();
    await f.close();
  }
});

test("Rust startNow cancels streaming, retains prior output and preserves the ordinary queue", async () => {
  const first = Promise.withResolvers<void>();
  const release = Promise.withResolvers<void>();
  const f = await fixture({
    async respond(_req, res, n) {
      if (n !== 1) {
        response(res, `answer-${n}`);
        return;
      }
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: "already visible" });
      first.resolve();
      await release.promise;
      if (!res.destroyed) {
        event(res, { content: "STALE_TEXT" });
        end(res, "stop");
      }
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "old" }));
    await first.promise;
    await h.wait((m) => JSON.stringify(m.params?.frame?.payload)?.includes("already visible"));
    const queued = h.envelope("sendText", id, { text: "future", requestedDelivery: "queue" });
    await h.command(queued);
    const now = h.envelope("sendText", id, { text: "now", requestedDelivery: "startNow" });
    const ack = await h.command(now);
    assert.equal(ack.status, "accepted");
    assert.equal((ack.result as Message).delivery, "startNow");
    assert.equal((await h.command(now)).status, "duplicate");
    await finished(h, queued.commandId);
    release.resolve();
    const rows = (await h.rows(id)).rows;
    assert.deepEqual(
      rows.filter((r) => r.kind === "userInput").map((r) => r.text),
      ["old", "now", "future"],
    );
    assert.equal(rows.find((r) => r.kind === "turnHeader")!.state, "completedInterrupted");
    assert(JSON.stringify(rows).includes("already visible"));
    assert(!JSON.stringify(rows).includes("STALE_TEXT"));
    assert.equal(f.requests.length, 3);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    release.resolve();
    await f.close();
  }
});

test(
  "Rust startNow waits for foreground Shell cancellation before the next request",
  { skip: process.platform === "win32" },
  async () => {
    let workdir = "";
    const f = await fixture({
      async respond(_req, res, n) {
        if (n > 1) {
          const pid = Number((await readFile(join(workdir, "shell.pid"), "utf8")).trim());
          assert.throws(() => process.kill(pid, 0), /ESRCH/);
          response(res);
          return;
        }
        res.writeHead(200, { "content-type": "text/event-stream" });
        event(res, {
          tool_calls: [
            {
              index: 0,
              id: "slow-shell",
              type: "function",
              function: {
                name: "Bash",
                arguments: JSON.stringify({
                  command: "echo $$ > shell.pid; sleep 30; echo leaked > leaked.txt",
                }),
              },
            },
          ],
        });
        end(res, "tool_calls");
      },
    });
    workdir = f.cwd;
    try {
      const h = f.start();
      const id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "old tools" }));
      await waitForFile(join(f.cwd, "shell.pid"));
      const now = h.envelope("sendText", id, { text: "new task", requestedDelivery: "startNow" });
      assert.equal((await h.command(now)).status, "accepted");
      await finished(h, now.commandId);
      assert.equal(f.requests.length, 2);
      const result = f.requests[1]!.messages.find((m: Message) => m.tool_call_id === "slow-shell");
      assert.match(result.content, /Interrupted/);
      await assert.rejects(readFile(join(f.cwd, "leaked.txt")), /ENOENT/);
      assert.deepEqual(h.schemaErrors, []);
    } finally {
      await f.close();
    }
  },
);

test("Rust followup mode persists, guide attachments fall back and stopped guides are held", async () => {
  const started = Promise.withResolvers<void>();
  const gate = Promise.withResolvers<void>();
  const f = await fixture({
    async respond(_req, res) {
      started.resolve();
      await gate.promise;
      if (!res.destroyed) response(res);
    },
  });
  try {
    await writeFile(join(f.cwd, "input.txt"), "attachment");
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    assert.equal(
      (await h.command(h.envelope("setFollowupMode", id, { mode: "guide" }))).status,
      "accepted",
    );
    await h.command(h.envelope("sendText", id, { text: "running" }));
    await started.promise;
    const guide = h.envelope("sendText", id, { text: "held guide" });
    assert.equal((await h.command(guide)).status, "accepted");
    const attachment = h.envelope("sendText", id, {
      text: "file guide",
      attachments: [
        { ref: join(f.cwd, "input.txt"), fileName: "input.txt", mime: "text/plain", bytes: 10 },
      ],
    });
    assert.equal((await h.command(attachment)).status, "accepted");
    const state = await snapshot(h, id);
    assert.equal(state.config.followupMode, "guide");
    assert.equal(state.inputRouting.mode, "guide");
    assert.equal(state.queue.items[0].delivery.admitted, "guide");
    assert.equal(state.queue.items[1].delivery.fallbackReasonCode, "guide.attachmentsUnsupported");
    const after = h.messages.length;
    await h.command(h.envelope("stop", id));
    await h.wait(
      (m) =>
        m.params?.frame?.payload?.deltas?.some(
          (d: Message) => d.patch?.control?.phase === "completedInterrupted",
        ),
      after,
    );
    const stopped = await snapshot(h, id, "stopped");
    assert.equal(stopped.inputRouting.mode, "choice");
    assert.equal(stopped.queue.items[0].delivery.fallbackReasonCode, "guide.turnInterrupted");
    assert.equal(stopped.queue.items[0].delivery.admitted, "queue");
    await h.close();
    const resumed = f.start();
    const restored = await snapshot(resumed, id);
    assert.equal(restored.config.followupMode, "guide");
    assert.equal(restored.queue.items.length, 0);
    assert.equal((await resumed.command(guide)).reasonCode, "fault.input.discardedOnRestart");
    assert.equal((await resumed.command(attachment)).reasonCode, "fault.input.discardedOnRestart");
    assert.deepEqual([...h.schemaErrors, ...resumed.schemaErrors], []);
  } finally {
    gate.resolve();
    await f.close();
  }
});

test("Rust guide freezes model selection until its durable continuation boundary", async () => {
  const gate = Promise.withResolvers<void>();
  const started = Promise.withResolvers<void>();
  const f = await fixture({
    registry: true,
    async respond(_req, res, n) {
      if (n === 1) {
        started.resolve();
        await gate.promise;
      }
      response(res);
    },
  });
  try {
    await configureRegistry(f);
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const first = h.envelope("sendText", id, { text: "first" });
    await h.command(first);
    await started.promise;
    assert.equal(
      (
        await h.command(
          h.envelope("sendText", id, {
            text: "new model guide",
            requestedDelivery: "guide",
            modelSelection: {
              providerId: "personal:fixture",
              modelId: "model-b",
              options: { reasoningLevel: "high" },
            },
          }),
        )
      ).status,
      "accepted",
    );
    assert.equal((await snapshot(h, id)).config.model, "model-a");
    gate.resolve();
    await finished(h, first.commandId);
    assert.deepEqual(
      f.requests.map((r) => [r.model, r.reasoning_effort]),
      [
        ["model-a", "low"],
        ["model-b", "high"],
      ],
    );
    assert.equal((await snapshot(h, id, "final")).config.model, "model-b");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    gate.resolve();
    await f.close();
  }
});

test("Rust failed model request falls pending guides back without consuming or losing them", async () => {
  const gate = Promise.withResolvers<void>();
  const started = Promise.withResolvers<void>();
  const f = await fixture({
    async respond(_req, res) {
      started.resolve();
      await gate.promise;
      res.writeHead(400, { "content-type": "application/json" });
      res.end(JSON.stringify({ error: { message: "invalid fixture request" } }));
    },
  });
  try {
    const h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "fail" }));
    await started.promise;
    const guide = h.envelope("sendText", id, { text: "preserve me", requestedDelivery: "guide" });
    assert.equal((await h.command(guide)).status, "accepted");
    gate.resolve();
    await h.wait((m) =>
      m.params?.frame?.payload?.deltas?.some((d: Message) => d.patch?.control?.phase === "error"),
    );
    const state = await snapshot(h, id);
    assert.equal(state.queue.items[0].sourceCommandId, guide.commandId);
    assert.equal(state.queue.items[0].delivery.fallbackReasonCode, "guide.turnInterrupted");
    assert.equal(state.inputRouting.mode, "choice");
    assert.equal((await h.rows(id)).rows.filter((r) => r.kind === "userInput").length, 1);
    assert.equal(f.requests.length, 1);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    gate.resolve();
    await f.close();
  }
});
