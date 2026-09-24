import assert from "node:assert/strict";
import test from "node:test";
import { readFile, writeFile, access } from "node:fs/promises";
import { join } from "node:path";
import {
  v4ConversationFileChangesResultSchema,
  v4ConversationFileRewindPreviewResultSchema,
} from "@zcode/shared/zcode-protocol-v4";
import { fixture, type Harness } from "./zcode-cli-rust-fixture.js";
type Message = Record<string, any>;
async function params(h: Harness, id: string) {
  const rows = await h.rows(id);
  const row = rows.rows.findLast((r) => r.kind === "userInput")!;
  return {
    sessionId: id,
    target: { rowId: row.rowId, entityId: row.entityId },
    baseRevision: rows.atRevision,
    baseLogEpoch: rows.atLogEpoch,
  };
}
async function command(h: Harness, id: string, type: string, payload: Message) {
  const p = await params(h, id);
  return {
    ...h.envelope(type, id, { target: p.target, ...payload }),
    baseRevision: p.baseRevision,
    baseLogEpoch: p.baseLogEpoch,
  };
}
async function preview(h: Harness, id: string) {
  return h.client.request(
    "v4/conversation/fileRewindPreview",
    await params(h, id),
    v4ConversationFileRewindPreviewResultSchema,
  );
}
test("Rust file rewind restores tracked files, refuses external changes, survives restart and is idempotent", async () => {
  const f = await fixture();
  try {
    let h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "write" }));
    await h.completed(id);
    const header = (await h.rows(id)).rows.findLast((r) => r.kind === "turnHeader")!;
    assert.deepEqual(header.fileChanges, { files: 1, additions: 1, deletions: 0, state: "active" });
    assert.equal(header.actions?.canRewindFiles, true);
    const details = await h.client.request(
      "v4/conversation/fileChanges",
      await params(h, id),
      v4ConversationFileChangesResultSchema,
    );
    assert.equal(details.items[0]?.writeCount, 1);
    assert.deepEqual(details.items[0]?.patches[0]?.lines, ["+written by Rust"]);
    const file = join(f.cwd, "result.txt"),
      safe = await preview(h, id);
    assert.equal(safe.canApply, true);
    assert.equal(safe.safeFiles[0]?.action, "delete");
    await writeFile(file, "external edit");
    const unsafe = await preview(h, id);
    assert.equal(unsafe.canApply, false);
    assert.equal(unsafe.unsafeFiles[0]?.reason, "external_modified");
    const conflict = await h.command(await command(h, id, "applyFileRewind", {}));
    assert.equal((conflict.result as Message).applied, false);
    assert.equal(await readFile(file, "utf8"), "external edit");
    await writeFile(file, "written by Rust");
    await h.close();
    // 重启后由 Node 的 workspace checkpoint entry 与产物恢复可撤销的文件变更（spec §5.5）。
    h = f.start();
    await h.subscribe(`conversation/${id}`);
    assert.equal(
      (await h.rows(id)).rows.findLast((r) => r.kind === "turnHeader")?.actions?.canRewindFiles,
      true,
    );
    const c = await command(h, id, "applyFileRewind", {});
    const ack = await h.command(c);
    assert.equal((ack.result as Message).applied, true);
    await assert.rejects(access(file));
    assert.equal((await h.command(c)).status, "duplicate");
    assert.equal((await preview(h, id)).safeFiles.length, 0);
    const restored = (await h.rows(id)).rows.findLast((r) => r.kind === "turnHeader")!;
    assert.equal(restored.fileChanges?.state, "reverted");
    assert.notEqual(restored.actions?.canRewindFiles, true);
    assert.equal(
      (
        await h.client.request(
          "v4/conversation/fileChanges",
          await params(h, id),
          v4ConversationFileChangesResultSchema,
        )
      ).state,
      "reverted",
    );
    assert(
      (await h.rows(id)).rows.some(
        (r) => r.kind === "timelineMarker" && r.marker.type === "checkpointRestored",
      ),
    );
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
test("Rust edit with workspace rewind restores files and cuts conversation before starting the replacement", async () => {
  const f = await fixture();
  try {
    const h = f.start(),
      id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "write" }));
    await h.completed(id);
    const c = await command(h, id, "editUserQuery", {
      newText: "after rewind",
      workspaceMode: "rewind",
    });
    const at = h.messages.length;
    const ack = await h.command(c);
    assert.equal((ack.result as Message).disposition, "rewind");
    await h.completed(id, at);
    await assert.rejects(access(join(f.cwd, "result.txt")));
    assert(!JSON.stringify(f.requests.at(-1)).includes("written by Rust"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Rust rewind commit failure compensates files and preserves original history", async () => {
  const { DatabaseSync } = await import("node:sqlite");
  const f = await fixture();
  try {
    let h = f.start();
    const id = await h.create();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "write" }));
    await h.completed(id);
    // 回退的分支切点（session.revert）与重跑输入同一事务；写入失败时撤销已恢复的文件。
    const db = new DatabaseSync(f.db);
    db.exec(
      "CREATE TRIGGER fail_rewind BEFORE UPDATE ON session WHEN new.revert IS NOT NULL BEGIN SELECT RAISE(ABORT,'injected history failure'); END;",
    );
    await assert.rejects(
      h.command(
        await command(h, id, "editUserQuery", {
          newText: "replacement must not run",
          workspaceMode: "rewind",
        }),
      ),
    );
    await h.close(1);
    assert.equal(await readFile(join(f.cwd, "result.txt"), "utf8"), "written by Rust");
    db.exec("DROP TRIGGER fail_rewind");
    db.close();
    h = f.start();
    await h.subscribe(`conversation/${id}`);
    const rows = await h.rows(id);
    assert(rows.rows.some((r) => r.kind === "userInput" && r.text === "write"));
    assert(!JSON.stringify(rows).includes("replacement must not run"));
    assert(!f.requests.some((req) => req.messages.at(-1).content === "replacement must not run"));
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
