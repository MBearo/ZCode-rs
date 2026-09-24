import assert from "node:assert/strict";
import test from "node:test";
import { join } from "node:path";
import { readFile } from "node:fs/promises";
import { DatabaseSync } from "node:sqlite";
import { zcodeSessionListResultSchema } from "@zcode/shared";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { seedList } from "./zcode-cli-rust-list-fixture.js";
import { repairSubagentTaskIndex } from "../src/zcode-agent/repairSubagentTaskIndex.js";

test("Rust session/list over the Node database matches Node identity mapping, archive/type filters, order and ID batches", async () => {
  const f = await fixture();
  try {
    const seed = await seedList(f);
    const h = f.start(seed.identity);
    const list = (p: unknown = {}) =>
      h.client.request("session/list", p, zcodeSessionListResultSchema);
    // Node `listSessions`：SQL 先按 directory、类型与归档取 limit 条（time_updated desc, id desc），
    // 再过滤工作区；其他工作区的会话会占用 limit（Node 缺陷，保持一致）。
    const listed = (limit: number) =>
      seed.records
        .filter(
          (r) =>
            r.directory === f.cwd &&
            !r.time.archived &&
            ["interactive", "fork", "workflow_parent"].includes(r.taskType),
        )
        .slice(0, limit)
        .filter((r) => r.workspaceID === seed.identity)
        .map((r) => seed.project(r));
    const visible = listed(1000);
    assert.deepEqual((await list({ workspace: seed.workspace })).sessions, listed(50));
    assert.equal(listed(50).length, 49);
    assert.deepEqual((await list({ workspace: seed.workspace, limit: 3 })).sessions, listed(3));
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
    // Node：请求不带 workspace 时按 `path ?? directory` 构造（buildWorkspaceRef），不带 identity。
    assert.deepEqual(
      global.map((r) => [r.sessionId, r.workspace.workspacePath, r.workspace.workspaceIdentity]),
      [
        ["child", f.cwd, undefined],
        ["other", f.cwd, undefined],
        ["local", f.cwd, undefined],
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
  const f = await fixture();
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

test("session/list reads metadata only without writing, and bounds frames and storage errors", async () => {
  const f = await fixture();
  try {
    const seed = await seedList(f);
    const h = f.start(seed.identity);
    const list = (p: unknown = {}) =>
      h.client.request("session/list", p, zcodeSessionListResultSchema);
    await list();
    const db = new DatabaseSync(f.db);
    try {
      // 列表只读会话行：损坏的对话记录不影响列表，也不触发任何写入。
      db.prepare(
        "INSERT INTO message(id,session_id,time_created,time_updated,data) VALUES ('broken','child',1,1,'intentionally invalid')",
      ).run();
      const version = db.prepare("PRAGMA data_version").get();
      assert.deepEqual(
        (await list({ workspace: seed.workspace, sessionIds: ["child"] })).sessions,
        [seed.project(seed.byId("child"))],
      );
      assert.deepEqual(db.prepare("PRAGMA data_version").get(), version);
      assert.equal(
        (await list({ sessionIds: ["different-path"] })).sessions[0]!.workspace.workspacePath,
        join(f.cwd, "actual"),
      );
      assert.equal((await list({ sessionIds: ["main-01"] })).sessions[0]!.traceId, undefined);
      db.prepare("UPDATE session SET title=? WHERE id='main-01'").run("界".repeat(310_000));
      await assert.rejects(list({ sessionIds: ["main-01"] }), /frame budget/);
      assert.equal((await list({ sessionIds: ["main-02"] })).sessions.length, 1);
      db.exec("ALTER TABLE session RENAME TO fixture_missing_sessions");
      try {
        await assert.rejects(list(), /no such table/);
      } finally {
        db.exec("ALTER TABLE fixture_missing_sessions RENAME TO session");
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
