import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { seedColdSessions } from "./zcode-cli-rust-node-db.js";

test("Rust startup reads only the session index; idle histories are evicted while subscribed history stays resident", async () => {
  const f = await fixture();
  try {
    let h = f.start();
    const original = await h.create();
    await h.subscribe(`conversation/${original}`);
    await h.command(h.envelope("sendText", original, { text: "seed" }));
    await h.completed(original);
    await h.close();
    await seedColdSessions(f, 300);
    // 若启动读取全部 transcript，这个未打开会话会直接令初始化失败。
    const db = new DatabaseSync(f.db);
    db.exec("UPDATE message SET data='not-json' WHERE session_id='cold-299'");
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
