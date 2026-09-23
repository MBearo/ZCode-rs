import assert from "node:assert/strict";
import test from "node:test";
import { mkdir, writeFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import { DatabaseSync } from "node:sqlite";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { SessionId, MessageId, PartId, ProjectId, WorkspaceId } from "@zcode/contracts";
import {
  v4AttachmentReadResultSchema,
  v4ConversationAttachmentReadResultSchema,
} from "@zcode/shared/zcode-protocol-v4";

test("TS task membership preserves visible forks while archived and auxiliary tasks remain addressable", async () => {
  const f = await fixture({ legacy: true });
  try {
    const { store, sessionID, messageID } = await seed(f);
    for (const [id, taskType] of [
      ["fork", "fork"],
      ["workflow", "workflow_parent"],
      ["child", "subagent_child"],
      ["archived", "interactive"],
    ] as const) {
      await store.createSession({
        id: id as SessionId,
        parentID: sessionID,
        taskType,
        projectID: "project" as ProjectId,
        workspaceID: f.cwd as WorkspaceId,
        directory: f.cwd,
        slug: id,
        title: id,
        version: "fixture",
      });
    }
    await store.updateSession({ id: "archived" as SessionId, timeArchived: 10 });
    const assistant = "planned-answer" as MessageId;
    await store.saveMessage({
      id: assistant,
      sessionID,
      role: "assistant",
      parentID: messageID,
      time: { created: 2 },
      agent: "main",
      mode: "plan",
      path: { cwd: f.cwd, root: f.cwd },
      cost: 0,
      tokens: { input: 1, output: 1, reasoning: 0, cache: { read: 0, write: 0 } },
    });
    await store.savePart({
      id: "old-plan" as PartId,
      sessionID,
      messageID: assistant,
      type: "tool",
      tool: "ExitPlanMode",
      callID: "plan-call",
      state: {
        status: "completed",
        input: { plan: "Historical plan remains readable" },
        output: "accepted",
        title: "Plan",
        metadata: {},
        time: { start: 2, end: 3 },
      },
    });
    store.close();
    for (let restart = 0; restart < 2; restart++) {
      const h = f.start();
      await h.subscribe(`sessions-index/${f.cwd}`);
      const frame = await h.wait((m) => m.params?.frame?.payload?.snapshot?.sessions);
      assert.deepEqual(
        frame.params.frame.payload.snapshot.sessions.map((s: any) => s.sessionId).sort(),
        ["fork", "legacy", "workflow"],
      );
      for (const id of ["fork", "child", "archived"]) await h.rows(id);
      for (const [id, kind] of [
        ["fork", "fork"],
        ["child", "subagent_child"],
        ["archived", "interactive"],
      ]) {
        const snapshot = await h.client.request(
          "session/read",
          { sessionId: id },
          zcodeSessionStateSnapshotSchema,
        );
        assert.equal(snapshot.session.sessionKind, kind);
        assert.equal(snapshot.session.parentSessionId, sessionID);
        assert.equal(snapshot.session.archivedAt, id === "archived" ? 10 : undefined);
      }
      const plans = await h.client.request(
        "v4/conversation/plans",
        { sessionId: sessionID },
        z.any(),
      );
      assert.equal(plans.plans.length, 1);
      assert.equal(plans.plans[0].toolCallId, "plan-call");
      assert.deepEqual(h.schemaErrors, []);
      await h.close();
    }
    assert.equal(f.requests.length, 0);
  } finally {
    await f.close();
  }
});

async function seed(f: Awaited<ReturnType<typeof fixture>>, planEnabled = false) {
  const store = createSqliteSessionStore({ dbPath: join(f.root, "ts.sqlite") });
  const sessionID = "legacy" as SessionId;
  const messageID = "input" as MessageId;
  await store.createSession({
    id: sessionID,
    projectID: "project" as ProjectId,
    workspaceID: f.cwd as WorkspaceId,
    directory: f.cwd,
    slug: "legacy",
    title: "legacy",
    version: "fixture",
  });
  await store.saveSessionEntry({
    id: "mode",
    sessionID,
    type: "runtime/execution_state",
    time: { created: 1, updated: 1 },
    data: { mode: "yolo", planEnabled },
  });
  await store.saveSessionEntry({
    id: "model",
    sessionID,
    type: "runtime/model_selection",
    time: { created: 1, updated: 1 },
    data: { providerId: "fixture", modelId: "core-model", options: { reasoningLevel: "none" } },
  });
  await store.saveMessage({
    id: messageID,
    sessionID,
    role: "user",
    time: { created: 1 },
    agent: "main",
  });
  await store.savePart({
    id: "text" as PartId,
    sessionID,
    messageID,
    type: "text",
    text: "historical question",
  });
  return { store, sessionID, messageID };
}
test("Imported artifact bytes survive TS cache removal and read queries enforce session and row authorization", async () => {
  const f = await fixture({ legacy: true });
  try {
    const { store, sessionID, messageID } = await seed(f);
    const storage = join(f.root, "storage");
    const uri = "zcode-artifact://legacy/tool-result-fixture";
    const data = Buffer.from("fixture-image-bytes");
    await mkdir(join(storage, "cli/artifacts/legacy"), { recursive: true });
    await writeFile(
      join(storage, "cli/artifacts/legacy/file-tool-result-fixture.txt"),
      `data:image/png;base64,${data.toString("base64")}`,
    );
    await writeFile(join(f.cwd, "zcode.json"), JSON.stringify({ storage: { dir: storage } }));
    await store.savePart({
      id: "image" as PartId,
      sessionID,
      messageID,
      type: "file",
      mime: "image/png",
      filename: "old.png",
      url: uri,
      metadata: { storageKind: "artifact", artifactUri: uri },
    });
    store.close();
    const h = f.start();
    await h.subscribe(`conversation/${sessionID}`);
    const rows = await h.rows(sessionID);
    const row = rows.rows.find((r: any) => r.kind === "userInput")!;
    const target = { rowId: row.rowId, entityId: row.entityId };
    await rm(storage, { recursive: true });
    const result = await h.client.request(
      "v4/attachment/read",
      { sessionId: sessionID, ref: uri, target, attachmentIndex: 0, offset: 0, limit: 4 },
      v4AttachmentReadResultSchema,
    );
    assert.equal(result.totalBytes, data.length);
    assert.equal(Buffer.from(result.dataBase64, "base64").toString(), "fixt");
    assert.equal(result.nextOffset, 4);
    const rest = await h.client.request(
      "v4/conversation/attachmentRead",
      { sessionId: sessionID, ref: uri, target, attachmentIndex: 0, offset: 4, limit: 512 },
      v4ConversationAttachmentReadResultSchema,
    );
    assert.equal(rest.nextOffset, null);
    await assert.rejects(
      h.client.request(
        "v4/attachment/read",
        {
          sessionId: sessionID,
          ref: uri,
          target: { ...target, entityId: "forged" },
          attachmentIndex: 0,
          offset: 0,
          limit: 4,
        },
        z.any(),
      ),
      /readNotAuthorized/,
    );
    const other = await h.create();
    await assert.rejects(
      h.client.request(
        "v4/attachment/read",
        { sessionId: other, ref: uri, offset: 0, limit: 4 },
        z.any(),
      ),
      /readNotAuthorized/,
    );
    await h.command(h.envelope("sendText", sessionID, { text: "continue" }));
    await h.completed(sessionID);
    assert.match(JSON.stringify(f.requests[0]!.messages), /data:image\/png;base64/);
    await h.close();
    const restarted = f.start();
    await restarted.subscribe(`conversation/${sessionID}`);
    const cold = await restarted.client.request(
      "v4/attachment/read",
      { sessionId: sessionID, ref: uri, offset: 0, limit: 512 },
      v4AttachmentReadResultSchema,
    );
    assert.equal(Buffer.from(cold.dataBase64, "base64").toString(), data.toString());
    assert.deepEqual(restarted.schemaErrors, []);
  } finally {
    await f.close();
  }
});
test("Imported plan state requires an explicit false input before yolo execution", async () => {
  const f = await fixture({ legacy: true });
  try {
    const { store, sessionID } = await seed(f, true);
    store.close();
    const h = f.start();
    await h.subscribe(`conversation/${sessionID}`);
    const snapshot = await h.wait(
      (m) => m.params?.frame?.payload?.snapshot?.sessionId === sessionID,
    );
    assert.equal(snapshot.params.frame.payload.snapshot.config.planEnabled, true);
    assert.equal(
      (await h.command(h.envelope("sendText", sessionID, { text: "blocked" }))).status,
      "rejected",
    );
    assert.equal(f.requests.length, 0);
    assert.equal(
      (await h.command(h.envelope("sendText", sessionID, { text: "execute", planEnabled: false })))
        .status,
      "accepted",
    );
    await h.completed(sessionID);
    assert.equal(f.requests.length, 1);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
test("Unknown TS history and a missing explicit source report storage failure before admitting any request", async () => {
  const f = await fixture({ legacy: true });
  try {
    const { store } = await seed(f);
    store.close();
    const db = new DatabaseSync(join(f.root, "ts.sqlite"));
    db.exec("UPDATE part SET data=json_set(data,'$.type','future-part')");
    db.close();
    const h = f.start();
    await h.wait((m) => m.method === "startup/storageState" && m.params.phase === "failed");
    await h.exited;
    await h.close(1);
    assert.match(h.stderr, /Unsupported TS history part/);
    assert.equal(f.requests.length, 0);
    await rm(join(f.root, "ts.sqlite"));
    const missing = f.start();
    await missing.wait((m) => m.method === "startup/storageState" && m.params.phase === "failed");
    await missing.exited;
    await missing.close(1);
    assert.match(missing.stderr, /Explicit TS import source does not exist/);
  } finally {
    await f.close();
  }
});
