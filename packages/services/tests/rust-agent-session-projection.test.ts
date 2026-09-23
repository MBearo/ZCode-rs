import assert from "node:assert/strict";
import test from "node:test";
import { join } from "node:path";
import { DatabaseSync } from "node:sqlite";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";
import { TaskIndexRepo } from "../src/session/taskIndexRepo.js";
import { fixture } from "./rust-agent-fixture.js";

test("Rust local and remote snapshots preserve workspace identity through cold reads", async () => {
  for (const identity of [undefined, "ssh:fixture:/project"]) {
    const f = await fixture();
    try {
      const h = f.start(identity),
        id = await h.create();
      await h.subscribe(`conversation/${id}`);
      await h.command(h.envelope("sendText", id, { text: "identity" }));
      await h.completed(id);
      const read = async (owner: typeof h) => {
        const snapshot = await owner.client.request(
          "session/read",
          { sessionId: id, deliveryKind: "desktop-continuous" },
          zcodeSessionStateSnapshotSchema,
        );
        assert.equal(snapshot.session.workspace.workspaceIdentity, identity);
        assert.equal(snapshot.session.workspace.workspacePath, f.cwd);
        assert.equal(snapshot.session.workspace.workspaceKey, identity ?? f.cwd);
      };
      await read(h);
      await h.close();
      await read(f.start(identity));
    } finally {
      await f.close();
    }
  }
});

test("Task index repairs legacy local path identity in read projections and preserves remote isolation and shell state", async () => {
  const f = await fixture();
  const path = join(f.root, "tasks-index.sqlite");
  const repo = new TaskIndexRepo(path);
  try {
    const local = {
      taskId: "same",
      traceId: "fixture",
      workspacePath: f.cwd,
      workspaceIdentity: f.cwd,
      title: "kept",
      mode: "yolo" as const,
      provider: "glm" as const,
      createdAt: 1,
      updatedAt: 2,
    };
    const remote = { ...local, workspaceIdentity: "ssh:fixture:/project", title: "remote" };
    await repo.syncTaskMeta({ meta: local });
    await repo.syncTaskMeta({ meta: remote });
    await repo.queryGroupedTaskView({ workspaceScopes: [local, remote] });
    const structure = await repo.queryGroupedTaskViewStructure({
      workspaceScopes: [local, remote],
    });
    assert.equal(structure.members.length, 2);
    assert.equal(
      structure.members.find((m) => m.workspaceKey === f.cwd)?.workspaceIdentity,
      undefined,
    );
    assert.equal(
      structure.members.find((m) => m.workspaceKey === remote.workspaceIdentity)?.workspaceIdentity,
      remote.workspaceIdentity,
    );
    await repo.updateTaskState({
      ...local,
      patch: { pinned: true, archived: true, title: "user title", titleOverridden: true },
    });
    assert.equal((await repo.getTaskMeta(local))?.workspaceIdentity, undefined);
    assert.equal((await repo.getTaskMeta(remote))?.workspaceIdentity, remote.workspaceIdentity);
    const db = new DatabaseSync(path);
    const stored = db
      .prepare("SELECT pinned,archived,title,title_overridden FROM tasks WHERE workspace_key=?")
      .get(f.cwd);
    assert.deepEqual(
      { ...stored },
      { pinned: 1, archived: 1, title: "user title", title_overridden: 1 },
    );
    db.close();
  } finally {
    repo.close();
    await f.close();
  }
});
