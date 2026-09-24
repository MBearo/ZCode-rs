import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { z } from "zod";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { sharedSnapshot } from "./zcode-cli-rust-shared-fixture.js";
import { seedColdSessions } from "./zcode-cli-rust-node-db.js";

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
    await seedColdSessions(f, 120);
    const db = new DatabaseSync(f.db);
    // 未打开会话的对话记录损坏：启动与索引只读会话行，不能因此失败。
    const corrupt = db.prepare(
      "INSERT INTO message(id,session_id,time_created,time_updated,data) VALUES (?,?,2,2,'unopened invalid JSON')",
    );
    for (let i = 0; i < 120; i++) corrupt.run(`msg_corrupt_${i}`, `cold-${i}`);
    const read = () => db.prepare("SELECT * FROM session WHERE id=?").get(sid);
    const original = read();
    for (const table of ["session", "message", "part"])
      db.exec(
        `CREATE TRIGGER forbid_eager_${table} BEFORE UPDATE ON ${table} BEGIN SELECT RAISE(ABORT,'eager session recovery'); END`,
      );
    const h = f.start();
    const sub = await h.subscribe(`sessions-index/${f.cwd}`);
    const frame = await h.wait(
      (m) =>
        m.params?.subscriptionId === sub.ack.subscriptionId &&
        m.params?.frame?.payload?.kind === "snapshot",
    );
    assert.equal(frame.params.frame.payload.snapshot.sessions.length, 121);
    assert.deepEqual(read(), original);
    const snapshot = await h.client.request(
      "session/read",
      { sessionId: sid, messageLimit: 1 },
      z.any(),
    );
    assert.equal(snapshot.messages.length, 1);
    assert.deepEqual(read(), original);
    assert.equal((await h.command(command)).status, "duplicate");
    assert.equal(f.requests.length, 1);
    for (const table of ["session", "message", "part"])
      db.exec(`DROP TRIGGER forbid_eager_${table}`);
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
