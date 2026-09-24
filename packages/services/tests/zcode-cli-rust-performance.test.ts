import assert from "node:assert/strict";
import test from "node:test";
import { DatabaseSync } from "node:sqlite";
import { join, delimiter } from "node:path";
import { mkdir, writeFile, access, symlink } from "node:fs/promises";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { SessionId, MessageId, PartId } from "@zcode/contracts";

test("Metadata-only commits do not rewrite unchanged transcript rows", async () => {
  const f = await fixture();
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "seed" }));
    await h.completed(id);
    // 与 Node 一样只追加：改标题只更新 session 行，不重写或追加 message/part。
    const db = new DatabaseSync(f.db);
    for (const table of ["message", "part"])
      for (const op of ["UPDATE", "INSERT"])
        db.exec(
          `CREATE TRIGGER no_${op}_${table} BEFORE ${op} ON ${table} BEGIN SELECT RAISE(ABORT,'transcript rewritten'); END;`,
        );
    assert.equal(
      (await h.command(h.envelope("renameSession", id, { title: "renamed" }))).status,
      "accepted",
    );
    assert.equal((await h.rows(id)).rows.filter((r) => r.kind === "userInput").length, 1);
    assert.equal(
      (db.prepare("SELECT title FROM session WHERE id=?").get(id) as { title: string }).title,
      "renamed",
    );
    db.close();
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("A single oversized idle session is evicted by bytes even below the count limit", async () => {
  const f = await fixture();
  try {
    let h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "seed" }));
    await h.completed(id);
    await h.close();
    const store = createSqliteSessionStore({ dbPath: f.db });
    for (let i = 0; i < 350; i++) {
      const messageID = `msg_big_${String(i).padStart(4, "0")}` as MessageId;
      await store.saveMessage({
        id: messageID,
        sessionID: id as SessionId,
        role: "user",
        time: { created: 1000 + i },
        agent: "main",
      });
      await store.savePart({
        id: `prt_big_${String(i).padStart(4, "0")}` as PartId,
        sessionID: id as SessionId,
        messageID,
        type: "text",
        text: "x".repeat(64 * 1024),
      });
    }
    store.close();
    h = f.start();
    const first = await h.rows(id),
      second = await h.rows(id);
    assert.notEqual(first.atLogEpoch, second.atLogEpoch);
    await h.subscribe(`conversation/${id}`);
    const pinned = (await h.rows(id)).atLogEpoch;
    assert.equal((await h.rows(id)).atLogEpoch, pinned);
    await h.client.request("v4/connection/flow", {
      connectionId: "fixture-desktop",
      state: "closed",
    });
    assert.notEqual((await h.rows(id)).atLogEpoch, pinned);
    assert.equal(f.requests.length, 1);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test(
  "Non-repository context skips Git spawn but explicit GIT_DIR still invokes discovery",
  { skip: process.platform === "win32" },
  async () => {
    for (const kind of ["absent", "explicit", "nested-symlink"]) {
      const env: Record<string, string> = {};
      const f = await fixture({ env });
      try {
        const bin = join(f.cwd, "bin"),
          marker = join(f.cwd, "git-invoked");
        await mkdir(bin);
        await writeFile(
          join(bin, "git"),
          '#!/bin/sh\nprintf called >> "$GIT_TEST_MARKER"\nexit 1\n',
          { mode: 0o755 },
        );
        env.PATH = bin + delimiter + process.env.PATH;
        env.GIT_TEST_MARKER = marker;
        if (kind === "explicit") env.GIT_DIR = join(f.cwd, "explicit-repo");
        if (kind === "nested-symlink") {
          const physical = join(f.root, "repo");
          await mkdir(join(physical, "nested"), { recursive: true });
          await writeFile(join(physical, ".git"), "gitdir: fixture");
          // Git 按物理 cwd 查找父目录；符号链接路径不能造成错误的非仓库判定。
          const { rename, rm } = await import("node:fs/promises");
          await rename(bin, join(f.root, "bin"));
          env.PATH = join(f.root, "bin") + delimiter + process.env.PATH;
          await rm(f.cwd, { recursive: true });
          await symlink(join(physical, "nested"), f.cwd, "dir");
        }
        const h = f.start(),
          id = await h.create();
        await h.subscribe(`conversation/${id}`);
        await h.command(h.envelope("sendText", id, { text: "seed" }));
        await h.completed(id);
        if (kind !== "absent") await access(marker);
        else await assert.rejects(access(marker));
        assert.deepEqual(h.schemaErrors, []);
      } finally {
        await f.close();
      }
    }
  },
);
