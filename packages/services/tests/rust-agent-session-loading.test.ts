import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { z } from "zod";
import { fixture } from "./rust-agent-fixture.js";
import { sharedSnapshot } from "./rust-agent-shared-fixture.js";

test("Startup and index read metadata only, isolate unopened corrupt history and do not rewrite sessions", async () => {
  const f = await fixture();
  try {
    const first = f.start();
    const sid = await first.create();
    await first.subscribe(`conversation/${sid}`);
    const command = first.envelope("sendText", sid, { text: "saved" });
    await first.command(command);
    await first.completed(sid);
    await first.close();
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    const original = db.prepare("SELECT body FROM rust_session WHERE id=?").get(sid)!
      .body as string;
    const row = JSON.parse(original);
    for (let i = 0; i < 120; i++) {
      const id = `cold-${i}`;
      db.prepare("INSERT INTO rust_session VALUES(?,?,?)").run(
        f.cwd,
        id,
        JSON.stringify({ ...row, id, title: id }),
      );
      db.prepare("INSERT INTO rust_message VALUES(?,?,0,?)").run(
        f.cwd,
        id,
        "unopened invalid JSON",
      );
    }
    db.prepare("INSERT INTO rust_command VALUES(?,?,?)").run(
      f.cwd,
      '["cold-0","unread-ack"]',
      "unopened invalid ACK",
    );
    db.exec(
      "CREATE TRIGGER forbid_eager_write BEFORE UPDATE ON rust_session BEGIN SELECT RAISE(ABORT,'eager session recovery'); END",
    );
    const h = f.start();
    const sub = await h.subscribe(`sessions-index/${f.cwd}`);
    const frame = await h.wait(
      (m) =>
        m.params?.subscriptionId === sub.ack.subscriptionId &&
        m.params?.frame?.payload?.kind === "snapshot",
    );
    assert.equal(frame.params.frame.payload.snapshot.sessions.length, 121);
    assert.equal(db.prepare("SELECT body FROM rust_session WHERE id=?").get(sid)!.body, original);
    const snapshot = await h.client.request(
      "session/read",
      { sessionId: sid, messageLimit: 1 },
      z.any(),
    );
    assert.equal(snapshot.messages.length, 1);
    assert.equal(db.prepare("SELECT body FROM rust_session WHERE id=?").get(sid)!.body, original);
    assert.equal((await h.command(command)).status, "duplicate");
    assert.equal(f.requests.length, 1);
    db.exec("DROP TRIGGER forbid_eager_write");
    await sharedSnapshot(h, sid);
    await assert.rejects(sharedSnapshot(h, "cold-0"));
    assert.equal((await sharedSnapshot(h, sid)).control.phase, "completedSuccess");
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
    db.close();
  } finally {
    await f.close();
  }
});

test("Unsubscribed history uses bounded LRU while both delivery subscriptions pin their session", async () => {
  const f = await fixture();
  try {
    const h = f.start();
    const sessions: string[] = [];
    const epochs = new Map<string, string>();
    for (let i = 0; i < 12; i++) {
      const sid = await h.create();
      sessions.push(sid);
      const sub = await h.subscribe(`conversation/${sid}`);
      epochs.set(sid, sub.ack.logEpoch);
      const offset = h.messages.length;
      await h.command(h.envelope("sendText", sid, { text: `message ${i}` }));
      await h.completed(sid, offset);
      if (i === 0) await h.subscribe(`conversation/${sid}`, "mobile", "web-remote-replayable");
      await h.client.request(
        "v4/conversation/unsubscribe",
        { connectionId: "fixture-desktop", subscriptionId: sub.ack.subscriptionId },
        z.unknown(),
      );
    }
    assert.equal((await sharedSnapshot(h, sessions[0]!)).logEpoch, epochs.get(sessions[0]!));
    assert.notEqual((await sharedSnapshot(h, sessions[1]!)).logEpoch, epochs.get(sessions[1]!));
    assert.equal(f.requests.length, 12);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    await f.close();
  }
});
