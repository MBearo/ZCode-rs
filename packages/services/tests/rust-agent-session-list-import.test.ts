import assert from "node:assert/strict";
import test from "node:test";
import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { zcodeSessionListResultSchema } from "@zcode/shared";
import { fixture } from "./rust-agent-fixture.js";
import { seedList } from "./rust-agent-list-fixture.js";
import { repairSubagentTaskIndex } from "../src/zcode-agent/repairSubagentTaskIndex.js";

test("Rust stored session/list matches TS identity mapping, archive/type filters, order and ID batches", async () => {
  const f = await fixture({ legacy: true });
  try {
    const seed = await seedList(f);
    for (const identity of [seed.otherIdentity, f.cwd]) {
      const imported = f.start(identity);
      await imported.client.request("session/list", {}, zcodeSessionListResultSchema);
      await imported.close();
    }
    const h = f.start(seed.identity);
    const list = (p: unknown = {}) =>
      h.client.request("session/list", p, zcodeSessionListResultSchema);
    const visible = seed.records
      .filter(
        (r) =>
          r.workspaceID === seed.identity &&
          r.directory === f.cwd &&
          !r.time.archived &&
          ["interactive", "fork", "workflow_parent"].includes(r.taskType),
      )
      .map((r) => seed.project(r));
    assert.deepEqual((await list({ workspace: seed.workspace })).sessions, visible.slice(0, 50));
    assert.deepEqual(
      (await list({ workspace: seed.workspace, limit: 3 })).sessions,
      visible.slice(0, 3),
    );
    assert.deepEqual((await list({ workspace: seed.workspace, limit: 1000 })).sessions, visible);
    const ids = [
      "child",
      "archived-child",
      "missing",
      "fork",
      "child",
      "other",
      "different-directory",
    ];
    for (const includeArchived of [false, true]) {
      const expected = ids
        .map(seed.byId)
        .filter(
          (r) => r && r.workspaceID === seed.identity && (includeArchived || !r.time.archived),
        )
        .map((r) => seed.project(r));
      assert.deepEqual(
        (await list({ workspace: seed.workspace, sessionIds: ids, includeArchived, limit: 1 }))
          .sessions,
        expected,
      );
    }
    const padded = Object.fromEntries(
      Object.entries(seed.workspace).map(([k, v]) => [k, `  ${v} \n`]),
    );
    assert.deepEqual((await list({ workspace: padded, sessionIds: [" child "] })).sessions, [
      seed.project(seed.byId("child")),
    ]);
    assert.deepEqual(
      (
        await list({
          workspace: { ...seed.workspace, workspaceIdentity: "unknown" },
          sessionIds: ["child"],
        })
      ).sessions,
      [],
    );
    const global = (await list({ includeArchived: true, sessionIds: ["child", "other", "local"] }))
      .sessions;
    assert.deepEqual(
      global.map((r) => [r.workspace.workspacePath, r.workspace.workspaceIdentity]),
      [
        [f.cwd, seed.identity],
        [f.cwd, seed.otherIdentity],
        [f.cwd, undefined],
      ],
    );
    assert.equal(global[0]!.traceId, "trace-child");
    const index = await h.subscribe(`sessions-index/${seed.identity}`);
    const frame = await h.wait(
      (m) =>
        m.params?.subscriptionId === index.ack.subscriptionId &&
        m.params.frame?.payload?.kind === "snapshot",
    );
    assert.equal(
      frame.params.frame.payload.snapshot.sessions.find((s: any) => s.sessionId === "main-01")
        .titleSource,
      "generated",
    );
    assert.equal((await list({ sessionIds: ["main-01"] })).sessions[0]!.titleSource, "first_input");
    assert.equal(f.requests.length, 0);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
    assert.deepEqual(await readFile(seed.source), seed.original);
  } finally {
    await f.close();
  }
});

test("Existing Host subagent index repair reads real Rust identities without deleting missing or stale-owner tasks", async () => {
  const f = await fixture({ legacy: true });
  try {
    const seed = await seedList(f);
    const h = f.start(seed.identity);
    const ids = ["child", "archived-child", "missing", "fork", "other", "workflow-child"];
    const updates: string[] = [];
    let removed = 0;
    let current = true;
    let invalidateOnResponse = false;
    type Params = Parameters<typeof repairSubagentTaskIndex>[0];
    const params: Params = {
      target: {
        workspacePath: f.cwd,
        workspaceIdentity: seed.identity,
        remoteSessionId: "connection-A",
      },
      visibleSessionIds: new Set(["main-00"]),
      isCurrent: () => current,
      onRemoved: () => {
        removed++;
      },
      taskIndexRepo: {
        listTaskMetas: async () =>
          ids.map((taskId) => ({ taskId })) as Awaited<
            ReturnType<Params["taskIndexRepo"]["listTaskMetas"]>
          >,
        updateTaskState: async (input) => {
          assert.deepEqual(input.patch, { deleted: true });
          updates.push(input.taskId);
          return { taskId: input.taskId } as Awaited<
            ReturnType<Params["taskIndexRepo"]["updateTaskState"]>
          >;
        },
      },
      agentService: {
        listSessions: async (input) => {
          assert.equal(input.runtimePolicy, "existing-only");
          assert.equal(input.workspaceIdentity, seed.identity);
          const result = await h.client.request(
            "session/list",
            {
              workspace: seed.workspace,
              sessionIds: input.sessionIds,
              includeArchived: input.includeArchived,
            },
            zcodeSessionListResultSchema,
          );
          if (invalidateOnResponse) current = false;
          return result.sessions;
        },
      },
    };
    await repairSubagentTaskIndex(params);
    assert.deepEqual(updates, ["child", "archived-child"]);
    assert.equal(removed, 2);
    updates.length = 0;
    invalidateOnResponse = true;
    await repairSubagentTaskIndex(params);
    assert.deepEqual(updates, []);
    assert.equal(f.requests.length, 0);
    await h.close();
  } finally {
    await f.close();
  }
});

test("session/list reads metadata only, never rewrites old identity, and bounds frames and storage errors", async () => {
  const f = await fixture({ legacy: true });
  try {
    const seed = await seedList(f);
    const h = f.start(seed.identity);
    const list = (p: unknown = {}) =>
      h.client.request("session/list", p, zcodeSessionListResultSchema);
    await list();
    const db = new DatabaseSync(join(f.dataDir, "rust-sessions.sqlite"));
    try {
      db.prepare(
        "UPDATE rust_session SET body=json_remove(body,'$.workspacePath','$.workspaceDirectory','$.traceId') WHERE id='child'",
      ).run();
      db.prepare("INSERT INTO rust_row(workspace,session,ordinal,body) VALUES (?,?,0,?)").run(
        seed.identity,
        "child",
        "intentionally invalid transcript",
      );
      const before = db.prepare("SELECT body FROM rust_session WHERE id='child'").get();
      const version = db.prepare("PRAGMA data_version").get();
      assert.deepEqual(
        (await list({ workspace: seed.workspace, sessionIds: ["child"] })).sessions,
        [seed.project(seed.byId("child"))],
      );
      assert.deepEqual(db.prepare("SELECT body FROM rust_session WHERE id='child'").get(), before);
      assert.deepEqual(db.prepare("PRAGMA data_version").get(), version);
      db.prepare(
        "UPDATE rust_session SET body=json_set(json_remove(body,'$.workspacePath','$.workspaceDirectory'),'$.promptSnapshot',json(?)) WHERE id='different-path'",
      ).run(JSON.stringify({ cwd: f.cwd }));
      assert.equal(
        (await list({ sessionIds: ["different-path"] })).sessions[0]!.workspace.workspacePath,
        join(f.cwd, "actual"),
      );
      const backup = db
        .prepare("SELECT backup FROM rust_legacy_import WHERE workspace=?")
        .get(seed.identity)!.backup;
      assert.equal(typeof backup, "string");
      db.prepare("UPDATE rust_legacy_import SET backup=? WHERE workspace=?").run(
        join(f.root, "missing-backup"),
        seed.identity,
      );
      // 已知空 trace 的新元数据不依赖 TS 备份可用性。
      assert.equal((await list({ sessionIds: ["main-01"] })).sessions[0]!.traceId, undefined);
      await assert.rejects(list({ sessionIds: ["child"] }));
      db.prepare("UPDATE rust_legacy_import SET backup=? WHERE workspace=?").run(
        backup as string,
        seed.identity,
      );
      db.prepare("UPDATE rust_session SET body=json_set(body,'$.title',?) WHERE id='main-01'").run(
        "界".repeat(310_000),
      );
      await assert.rejects(list({ sessionIds: ["main-01"] }), /frame budget/);
      assert.equal((await list({ sessionIds: ["main-02"] })).sessions.length, 1);
      db.exec("ALTER TABLE rust_session RENAME TO fixture_missing_sessions");
      try {
        await assert.rejects(list(), /no such table/);
      } finally {
        db.exec("ALTER TABLE fixture_missing_sessions RENAME TO rust_session");
      }
    } finally {
      db.close();
    }
    assert.equal(f.requests.length, 0);
    assert.deepEqual(h.schemaErrors, []);
    await h.close();
  } finally {
    await f.close();
  }
});
