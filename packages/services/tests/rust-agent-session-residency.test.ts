import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { join } from "node:path";
import { fixture } from "./rust-agent-fixture.js";

test("Rust startup reads only the session index; idle histories are evicted while subscribed history stays resident", async () => {
  const f = await fixture();
  try {
    let h = f.start();
    const original = await h.create();
    await h.subscribe(`conversation/${original}`);
    await h.command(h.envelope("sendText", original, { text: "seed" }));
    await h.completed(original);
    await h.close();
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    const session = db.prepare(
      "INSERT INTO rust_session SELECT workspace,?,json_set(body,'$.id',?) FROM rust_session WHERE id=?",
    );
    db.exec("BEGIN");
    for (let i = 0; i < 300; i++) {
      const id = `cold-${i}`;
      session.run(id, id, original);
      for (const table of ["rust_row", "rust_message"]) {
        db.prepare(
          `INSERT INTO ${table} SELECT workspace,?,ordinal,body FROM ${table} WHERE session=?`,
        ).run(id, original);
      }
      db.prepare(
        "INSERT INTO rust_history SELECT workspace,?,kind,ordinal,body FROM rust_history WHERE session=?",
      ).run(id, original);
    }
    // 若启动读取全部 transcript，这个未打开会话会直接令初始化失败。
    db.exec("UPDATE rust_message SET body='not-json' WHERE session='cold-299'");
    db.exec("COMMIT");
    db.close();
    h = f.start();
    await h.subscribe(`sessions-index/${f.cwd}`);
    await h.subscribe("conversation/cold-0");
    const pinnedEpoch = (await h.rows("cold-0")).atLogEpoch;
    const idleEpoch = (await h.rows("cold-1")).atLogEpoch;
    for (let i = 2; i < 14; i++) await h.rows(`cold-${i}`);
    assert.equal((await h.rows("cold-0")).atLogEpoch, pinnedEpoch);
    assert.notEqual((await h.rows("cold-1")).atLogEpoch, idleEpoch);
    await assert.rejects(h.rows("cold-299"));
    assert.equal((await h.rows("cold-0")).atLogEpoch, pinnedEpoch);
    assert.equal(f.requests.length, 1);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    await f.close();
  }
});
