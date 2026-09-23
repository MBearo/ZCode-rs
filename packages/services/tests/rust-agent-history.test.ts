import assert from "node:assert/strict";
import test from "node:test";
import { fixture, event, end, type Harness } from "./rust-agent-fixture.js";
import { setTimeout as delay } from "node:timers/promises";
type Message = Record<string, any>;
const target = (row: Message) => ({ rowId: row.rowId, entityId: row.entityId });
async function action(h: Harness, id: string, type: string, payload: Message) {
  const rows = await h.rows(id);
  return {
    ...h.envelope(type, id, payload),
    baseRevision: rows.atRevision,
    baseLogEpoch: rows.atLogEpoch,
  };
}
async function send(h: Harness, id: string, text: string) {
  const at = h.messages.length;
  const command = h.envelope("sendText", id, { text });
  assert.equal((await h.command(command)).status, "accepted");
  await done(h, id, command.commandId, at);
  return command;
}
async function done(h: Harness, id: string, commandId: string, after: number) {
  await h.wait(
    (m) =>
      m.params?.topic === `conversation/${id}` &&
      m.params.frame?.payload?.deltas?.some(
        (d: Message) =>
          d.row?.kind === "turnHeader" &&
          d.row?.sourceCommandId === commandId &&
          d.row?.state === "completedSuccess",
      ),
    after,
  );
}
test("Rust history retry/edit cut canonical history durably, retain command receipts and never reuse row IDs", async () => {
  const f = await fixture({
    respond(req, res, n) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: `answer-${n}` });
      end(res, "stop");
    },
  });
  try {
    let h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.subscribe(`conversation/${id}`, "phone", "web-remote-replayable");
    await send(h, id, "first");
    const original = await send(h, id, "second");
    let rows = await h.rows(id);
    const last = rows.rows.findLast((r) => r.kind === "assistantText")!;
    assert.equal(last.actions?.canRetry, true, JSON.stringify(rows));
    const retry = await action(h, id, "retryTurn", { target: target(last) });
    let at = h.messages.length;
    assert.equal((await h.command(retry)).status, "accepted");
    await done(h, id, retry.commandId, at);
    assert.equal((await h.command(retry)).status, "duplicate");
    assert.equal(f.requests.at(-1)!.messages.at(-1).content, "second");
    assert(!JSON.stringify(f.requests.at(-1)).includes("answer-2"));
    rows = await h.rows(id);
    assert(rows.rows.at(-1)!.rowId > last.rowId);
    assert.equal(rows.rows.filter((r) => r.kind === "userInput").length, 2);
    const user = rows.rows.findLast((r) => r.kind === "userInput")!;
    const edit = await action(h, id, "editUserQuery", { target: target(user), newText: "changed" });
    at = h.messages.length;
    const ack = await h.command(edit);
    assert.equal(ack.status, "accepted");
    assert.equal((ack.result as Message).disposition, "rewind");
    await done(h, id, edit.commandId, at);
    assert.equal(f.requests.at(-1)!.messages.at(-1).content, "changed");
    assert(!f.requests.at(-1)!.messages.some((m: Message) => m.content === "second"));
    assert(
      h.messages.slice(at).filter((m) => m.params?.frame?.payload?.kind === "snapshot").length >= 2,
    );
    await h.close();
    h = f.start();
    await h.subscribe(`conversation/${id}`);
    assert.equal((await h.command(original)).status, "duplicate");
    assert.equal((await h.command(retry)).status, "duplicate");
    assert.equal((await h.command(edit)).status, "duplicate");
    const cold = await h.rows(id);
    assert(!JSON.stringify(cold.rows).includes("second"));
    assert(cold.rows.some((r) => r.kind === "userInput" && r.text === "changed"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
test("Rust forks exact stable response while parent runs, rejects stale and nonlatest targets before cancelling", async () => {
  const f = await fixture({
    async respond(req, res) {
      if (req.messages.at(-1).content === "slow") await delay(350);
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, { content: "answer" });
      end(res, "stop");
    },
  });
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "first");
    const first = await h.rows(id);
    const stable = first.rows.find((r) => r.kind === "assistantText")!;
    const at = h.messages.length;
    await h.command(h.envelope("sendText", id, { text: "slow" }));
    const oldUser = first.rows.find((r) => r.kind === "userInput")!;
    assert.equal(
      (
        await h.command(
          await action(h, id, "editUserQuery", { target: target(oldUser), newText: "bad" }),
        )
      ).reasonCode,
      "guard.latestQueryEditOnly",
    );
    const fork = await h.command(await action(h, id, "forkAssistant", { target: target(stable) }));
    assert.equal(fork.status, "accepted");
    const child = (fork.result as Message).sessionId;
    await h.subscribe(`conversation/${child}`);
    await h.completed(id, at);
    await send(h, child, "fork input");
    assert(!JSON.stringify(f.requests.at(-1)).includes('"slow"'));
    assert(JSON.stringify(f.requests.at(-1)).includes('"first"'));
    const stale = {
      ...h.envelope("retryTurn", id, { target: target(stable) }),
      baseRevision: first.atRevision,
      baseLogEpoch: first.atLogEpoch,
    };
    assert.equal((await h.command(stale)).status, "stale");
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust history reruns frozen attachments after source deletion and explicit empty attachments clears them", async () => {
  const { writeFile, rm } = await import("node:fs/promises");
  const { join } = await import("node:path");
  const f = await fixture();
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const path = join(f.cwd, "original.txt");
    await writeFile(path, "FROZEN_HISTORY_ATTACHMENT");
    await h.command(
      h.envelope("sendText", id, {
        text: "read it",
        attachments: [{ ref: path, fileName: "original.txt", mime: "text/plain", bytes: 25 }],
      }),
    );
    await h.completed(id);
    await rm(path);
    for (const [type, extra] of [
      ["retryTurn", {}],
      ["editUserQuery", { newText: "changed" }],
      ["editUserQuery", { newText: "cleared", attachments: [] }],
    ] as const) {
      const rows = await h.rows(id);
      const row = rows.rows.findLast(
        (r) => r.kind === (type === "retryTurn" ? "assistantText" : "userInput"),
      )!;
      const c = await action(h, id, type, { target: target(row), ...extra });
      const at = h.messages.length;
      await h.command(c);
      await done(h, id, c.commandId, at);
      assert.equal(
        JSON.stringify(f.requests.at(-1)).includes("FROZEN_HISTORY_ATTACHMENT"),
        !("attachments" in extra),
      );
    }
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust editing a Goal preserves canonical Goal intent and a failed fork transaction creates no child", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const { join } = await import("node:path");
  const f = await fixture({
    respond(req, res) {
      res.writeHead(200, { "content-type": "text/event-stream" });
      event(res, {
        content: req.messages.at(-1).content.includes("Verify whether")
          ? '{"passed":true,"reason":"done","nextAction":""}'
          : "done",
      });
      end(res, "stop");
    },
  });
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    const initial = h.envelope("sendGoalCommand", id, { text: "original goal" });
    await h.command(initial);
    await done(h, id, initial.commandId, 0);
    const user = (await h.rows(id)).rows.findLast((r) => r.kind === "userInput")!;
    const edit = await action(h, id, "editUserQuery", {
      target: target(user),
      newText: "changed goal",
    });
    const at = h.messages.length;
    await h.command(edit);
    await done(h, id, edit.commandId, at);
    assert.match(JSON.stringify(f.requests.at(-1)), /changed goal/);
    assert(!JSON.stringify(f.requests.at(-1)).includes("original goal"));
    const rows = (await h.rows(id)).rows;
    assert(rows.some((r) => r.kind === "userInput" && r.text === "/goal changed goal"));
    const stable = rows.findLast((r) => r.kind === "assistantText")!;
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    db.exec(
      "CREATE TRIGGER fail_fork BEFORE INSERT ON rust_command WHEN json_extract(new.ack,'$.result.type')='forkAssistant' BEGIN SELECT RAISE(ABORT,'injected fork failure'); END;",
    );
    await assert.rejects(
      h.command(await action(h, id, "forkAssistant", { target: target(stable) })),
    );
    await h.close(1);
    assert.equal(
      (db.prepare("SELECT count(*) AS n FROM rust_session").get() as { n: number }).n,
      1,
    );
    db.close();
  } finally {
    await f.close();
  }
});

test("Corrupt persisted history boundaries are rejected before activating or replaying a session", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const { join } = await import("node:path");
  const f = await fixture();
  try {
    let h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await send(h, id, "first");
    await h.close();
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    db.exec("UPDATE rust_history SET body=json_set(body,'$.state.selection.provider',null)");
    db.close();
    h = f.start();
    await assert.rejects(h.subscribe(`conversation/${id}`), /Invalid persisted history boundary/);
    const healthy = await h.create();
    assert.notEqual(healthy, id);
    assert.equal(f.requests.length, 1);
    await h.close();
  } finally {
    await f.close();
  }
});
