import assert from "node:assert/strict";
import test from "node:test";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { SessionId, ProjectId } from "@zcode/contracts";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";

// Todo 与 Node 共用 `todo` 表（spec rust-m11-node-storage §4）：Node 写入的列表由 Rust 直接读取。
test("Rust reads the Todo lists Node wrote through session/read and the App plan", async () => {
  const f = await fixture();
  try {
    const store = createSqliteSessionStore({ dbPath: f.db });
    for (const id of ["first", "second"]) {
      await store.createSession({
        id: id as SessionId,
        projectID: "fixture" as ProjectId,
        directory: f.cwd,
        slug: id,
        title: id,
        version: "fixture",
      });
      await store.updateTodos({
        sessionID: id as SessionId,
        todos:
          id === "first"
            ? [
                { content: "first original", status: "in_progress", priority: "high" },
                { content: "next", status: "pending", priority: "low" },
              ]
            : [],
      });
    }
    store.close();
    const h = f.start();
    const sub = await h.subscribe("conversation/first");
    const frame = await h.wait(
      (m) =>
        m.params?.subscriptionId === sub.ack.subscriptionId &&
        m.params?.frame?.payload?.kind === "snapshot",
    );
    assert.deepEqual(
      frame.params.frame.payload.snapshot.plan?.items.map((i: any) => [i.content, i.status]),
      [
        ["first original", "inProgress"],
        ["next", "pending"],
      ],
    );
    const read = (sessionId: string) =>
      h.client.request("session/read", { sessionId }, zcodeSessionStateSnapshotSchema);
    assert.deepEqual((await read("first")).todos, [
      { content: "first original", status: "in_progress", priority: "high" },
      { content: "next", status: "pending", priority: "low" },
    ]);
    assert.deepEqual((await read("second")).todos ?? [], []);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    await f.close();
  }
});
