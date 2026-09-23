import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { readFile } from "node:fs/promises";
import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { CreateSessionInput, SessionId, SessionTaskType } from "@zcode/contracts";
import type { fixture } from "./rust-agent-fixture.js";

export async function seedList(f: Awaited<ReturnType<typeof fixture>>) {
  const source = join(f.root, "ts.sqlite");
  const store = createSqliteSessionStore({ dbPath: source });
  const identity = "remote-fixture-A";
  const otherIdentity = "remote-fixture-B";
  const workspace = {
    workspacePath: f.cwd,
    workspaceIdentity: identity,
    workspaceKey: identity,
    remoteSessionId: "connection-A",
  };
  const definitions: {
    id: string;
    kind?: SessionTaskType;
    identity?: string;
    directory?: string;
    path?: string;
    archived?: boolean;
  }[] = [
    ...Array.from({ length: 55 }, (_, i) => ({ id: `main-${String(i).padStart(2, "0")}` })),
    { id: "fork", kind: "fork" },
    { id: "workflow", kind: "workflow_parent" },
    { id: "child", kind: "subagent_child" },
    { id: "workflow-child", kind: "workflow_child" },
    { id: "nested-child", kind: "nested_workflow_child" },
    { id: "selection", kind: "selection_side_chat" },
    { id: "archived-main", archived: true },
    { id: "archived-child", kind: "subagent_child", archived: true },
    { id: "other", identity: otherIdentity },
    { id: "local", identity: f.cwd },
    { id: "different-directory", directory: join(f.cwd, "subdir") },
    { id: "different-path", path: join(f.cwd, "actual") },
  ];
  for (const d of definitions) {
    await store.createSession({
      id: d.id,
      projectID: "fixture",
      workspaceID: d.identity ?? identity,
      directory: d.directory ?? f.cwd,
      path: d.path ?? f.cwd,
      slug: d.id,
      title: `title ${d.id}`,
      titleSource: d.id === "fork" ? "custom" : "first_input",
      version: "fixture",
      taskType: d.kind ?? "interactive",
      parentID: d.kind ? "main-00" : undefined,
      traceID: d.id === "main-01" ? undefined : `trace-${d.id}`,
      time: { created: 100, updated: 200 },
    } as CreateSessionInput);
  }
  store.close();
  // 测试数据库 fixture 直接设置相同排序时间，避免 wall clock 干扰排序与归档断言。
  const db = new DatabaseSync(source);
  db.prepare("UPDATE session SET time_archived=300 WHERE id LIKE 'archived-%'").run();
  db.close();
  const oracle = createSqliteSessionStore({ dbPath: source });
  const records = await oracle.listSessions({ includeArchived: true, limit: 1000 });
  oracle.close();
  const { mapSessionInfo } = await import(
    new URL(
      "../../../apps/zcode-cli/packages/bootstrap/src/zcode-protocol/session-mapper.ts",
      import.meta.url,
    ).href
  );
  const project = (record: (typeof records)[number], w: typeof workspace = workspace) =>
    JSON.parse(JSON.stringify(mapSessionInfo({ session: record, workspace: w })));
  return {
    source,
    original: await readFile(source),
    identity,
    otherIdentity,
    workspace,
    records,
    project,
    byId: (id: string) => records.find((r) => r.id === (id as SessionId))!,
  };
}
