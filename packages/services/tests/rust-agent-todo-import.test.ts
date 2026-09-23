import assert from "node:assert/strict";
import test from "node:test";
import { readFile, readdir } from "node:fs/promises";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { fixture } from "./rust-agent-fixture.js";
import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { SessionId, ProjectId, WorkspaceId } from "@zcode/contracts";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";

async function seed(f: Awaited<ReturnType<typeof fixture>>) {
  const source = join(f.root, "ts.sqlite");
  const store = createSqliteSessionStore({ dbPath: source });
  for (const id of ["first", "second"]) {
    await store.createSession({
      id: id as SessionId,
      projectID: "fixture" as ProjectId,
      workspaceID: f.cwd as WorkspaceId,
      directory: f.cwd,
      slug: id,
      title: id,
      version: "fixture",
    });
    await store.saveSessionEntry({
      id: `${id}-mode`,
      sessionID: id as SessionId,
      type: "runtime/execution_state",
      time: { created: 1, updated: 1 },
      data: { mode: "yolo", planEnabled: false },
    });
    await store.updateTodos({
      sessionID: id as SessionId,
      todos: [
        { content: `${id} original`, status: "in_progress", priority: "high" },
        { content: "next", status: "pending", priority: "low" },
      ],
    });
  }
  store.close();
  return source;
}
function state(f: Awaited<ReturnType<typeof fixture>>, id: string) {
  const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"), { readOnly: true });
  try {
    return JSON.parse(
      db.prepare("SELECT body FROM rust_session WHERE id=?").get(id)!.body as string,
    );
  } finally {
    db.close();
  }
}

test("Rust imports authoritative TS Todo lists and backfills missing native fields from the committed backup only", async () => {
  const f = await fixture({ legacy: true });
  try {
    const source = await seed(f);
    const original = await readFile(source);
    const h = f.start();
    await h.subscribe("conversation/first");
    const read = await h.client.request(
      "session/read",
      { sessionId: "first" },
      zcodeSessionStateSnapshotSchema,
    );
    assert.equal(read.todos?.[0]?.content, "first original");
    await h.close();
    assert.deepEqual(await readFile(source), original);
    const files = (await readdir(f.dataDir)).filter((name) => name.startsWith("ts-backup-"));
    assert.equal(files.length, 1);
    const backup = await readFile(join(f.dataDir, files[0]!));
    // 模拟旧版本已导入但未保存 todos；另一会话的显式清空必须保留。
    const native = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    native
      .prepare(
        "UPDATE rust_session SET body=json_remove(body,'$.todos','$.todosUpdatedAt') WHERE id='first'",
      )
      .run();
    native
      .prepare("UPDATE rust_session SET body=json_set(body,'$.todos',json('[]')) WHERE id='second'")
      .run();
    native.close();
    const current = createSqliteSessionStore({ dbPath: source });
    await current.updateTodos({
      sessionID: "first" as SessionId,
      todos: [
        {
          content: "new TS state must not replace imported snapshot",
          status: "completed",
          priority: "low",
        },
      ],
    });
    current.close();
    const changedSource = await readFile(source);
    for (let i = 0; i < 2; i++) {
      const cold = f.start();
      await cold.subscribe("conversation/first");
      const first = await cold.client.request(
        "session/read",
        { sessionId: "first" },
        zcodeSessionStateSnapshotSchema,
      );
      assert.deepEqual(first.todos, read.todos);
      await cold.close();
      assert.deepEqual(state(f, "second").todos, []);
      assert.deepEqual(await readFile(source), changedSource);
      assert.deepEqual(await readFile(join(f.dataDir, files[0]!)), backup);
      assert.deepEqual(
        (await readdir(f.dataDir)).filter((name) => name.startsWith("ts-backup-")),
        files,
      );
    }
  } finally {
    await f.close();
  }
});

test("Rust rejects invalid stored TS Todo state and rolls back every imported session", async () => {
  const f = await fixture({ legacy: true });
  try {
    const source = await seed(f);
    const db = new DatabaseSync(source);
    db.prepare("UPDATE todo SET content='' WHERE session_id='second'").run();
    db.close();
    const original = await readFile(source);
    const h = f.start();
    await h.wait((m) => m.method === "startup/storageState" && m.params.phase === "failed");
    await h.close(1);
    assert.match(h.stderr, /Todo content must not be empty/);
    const native = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"), { readOnly: true });
    assert.equal(native.prepare("SELECT count(*) AS n FROM rust_session").get()!.n, 0);
    assert.equal(native.prepare("SELECT count(*) AS n FROM rust_legacy_import").get()!.n, 0);
    native.close();
    assert.deepEqual(await readFile(source), original);
    assert.deepEqual(
      (await readdir(f.dataDir)).filter((p) => /^ts-(backup|import)-/.test(p)),
      [],
    );
  } finally {
    await f.close();
  }
});
