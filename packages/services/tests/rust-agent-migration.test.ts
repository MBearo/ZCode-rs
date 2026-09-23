import assert from "node:assert/strict";
import test from "node:test";
import { readFile, readdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { DatabaseSync } from "node:sqlite";
import { fixture } from "./rust-agent-fixture.js";
import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { SessionId, MessageId, ProjectId, WorkspaceId, PartId } from "@zcode/contracts";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";

test("Real TS storage imports identity, attachments, tools and interrupted outcomes once; source remains usable for rollback", async () => {
  const f = await fixture({ legacy: true });
  try {
    const path = join(f.root, "ts.sqlite");
    const store = createSqliteSessionStore({ dbPath: path });
    const id = "ts-session" as SessionId;
    const user = "ts-user" as MessageId;
    const assistant = "ts-assistant" as MessageId;
    await store.createSession({
      id,
      projectID: "fixture-project" as ProjectId,
      workspaceID: f.cwd as WorkspaceId,
      directory: f.cwd,
      slug: "fixture",
      title: "Old TS task",
      titleSource: "custom",
      version: "fixture",
    });
    await store.saveSessionEntry({
      id: "selection",
      sessionID: id,
      type: "runtime/model_selection",
      time: { created: 1, updated: 1 },
      data: { providerId: "fixture", modelId: "core-model", options: { reasoningLevel: "none" } },
    });
    await store.saveSessionEntry({
      id: "mode",
      sessionID: id,
      type: "runtime/execution_state",
      time: { created: 1, updated: 1 },
      data: { mode: "yolo", planEnabled: false },
    });
    await store.saveMessage({
      id: user,
      sessionID: id,
      role: "user",
      time: { created: 10 },
      agent: "main",
      modelSelection: {
        providerId: "fixture",
        modelId: "core-model",
        options: { reasoningLevel: "none" },
      },
    });
    await store.savePart({
      id: "user-text" as PartId,
      sessionID: id,
      messageID: user,
      type: "text",
      text: "old question",
    });
    const attachment = join(f.root, "old.txt");
    await writeFile(attachment, "old attachment content");
    await store.savePart({
      id: "old-file" as PartId,
      sessionID: id,
      messageID: user,
      type: "file",
      mime: "text/plain",
      filename: "old.txt",
      url: pathToFileURL(attachment).href,
    });
    await store.saveMessage({
      id: assistant,
      sessionID: id,
      role: "assistant",
      parentID: user,
      time: { created: 20 },
      agent: "main",
      mode: "yolo",
      path: { cwd: f.cwd, root: f.cwd },
      cost: 0,
      tokens: { input: 10, output: 2, reasoning: 1, cache: { read: 0, write: 0 } },
    });
    await store.savePart({
      id: "assistant-text" as PartId,
      sessionID: id,
      messageID: assistant,
      type: "text",
      text: "old answer",
    });
    await store.savePart({
      id: "read" as PartId,
      sessionID: id,
      messageID: assistant,
      type: "tool",
      callID: "old-read",
      tool: "Read",
      declarationIndex: 0,
      state: {
        status: "completed",
        input: { file_path: "old.txt" },
        output: "stored file result",
        title: "read",
        metadata: {},
        time: { start: 20, end: 21 },
      },
    });
    await store.savePart({
      id: "write" as PartId,
      sessionID: id,
      messageID: assistant,
      type: "tool",
      callID: "old-write",
      tool: "Write",
      declarationIndex: 1,
      state: {
        status: "running",
        input: { file_path: "must-not-exist.txt", content: "not replayed" },
        time: { start: 22 },
      },
    });
    store.close();
    const before = await readFile(path);
    const h = f.start();
    await h.subscribe(`conversation/${id}`);
    const snapshot = await h.client.request(
      "session/read",
      { sessionId: id },
      zcodeSessionStateSnapshotSchema,
    );
    assert.equal(snapshot.session.title, "Old TS task");
    assert.ok(
      snapshot.messages
        .flatMap((m) => m.parts)
        .some((p) => p.type === "file" && p.filename === "old.txt" && p.mime === "text/plain"),
    );
    assert.ok(
      snapshot.messages
        .flatMap((m) => m.parts)
        .some((p) => p.type === "tool" && p.callId === "old-write" && p.state.status === "error"),
    );
    const rows = await h.rows(id);
    assert.ok(
      rows.rows.some(
        (r: any) => r.kind === "userInput" && r.attachments?.[0].fileName === "old.txt",
      ),
    );
    assert.ok(
      rows.rows.some(
        (r: any) =>
          r.kind === "toolCall" && r.toolCallId === "old-write" && r.status === "cancelled",
      ),
    );
    await h.command(h.envelope("sendText", id, { text: "continue old task" }));
    await h.completed(id);
    assert.match(JSON.stringify(f.requests[0]!.messages), /old attachment content/);
    assert.match(JSON.stringify(f.requests[0]!.messages), /execution outcome is unknown/);
    assert.deepEqual(
      f.requests[0]!.messages.filter((m: any) => m.role === "tool").map((m: any) => m.tool_call_id),
      ["old-read", "old-write"],
    );
    await h.close();
    const h2 = f.start();
    await h2.subscribe(`conversation/${id}`);
    const restored = await h2.rows(id);
    assert.equal(restored.rows.filter((r: any) => r.entityId === "ts-user").length, 2);
    await h2.close();
    assert.deepEqual(await readFile(path), before);
    const files = await readdir(f.dataDir);
    assert.equal(
      files.filter((p) => p.startsWith("ts-backup-") && p.endsWith(".sqlite")).length,
      1,
    );
    const source = new DatabaseSync(path, { readOnly: true });
    assert.equal((source.prepare("SELECT count(*) AS n FROM message").get() as { n: number }).n, 2);
    source.close();
    assert.deepEqual(h.schemaErrors, []);
    assert.deepEqual(h2.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("TS migration preserves workspace identity, blocks non-yolo execution and retains discarded input ACKs", async () => {
  const f = await fixture({ legacy: true });
  try {
    const store = createSqliteSessionStore({ dbPath: join(f.root, "ts.sqlite") });
    for (const [id, workspace, mode] of [
      ["local", f.cwd, "build"],
      ["remote", "ssh://fixture/workspace", "yolo"],
    ]) {
      const sessionID = id as SessionId;
      const messageID = `${id}-user` as MessageId;
      await store.createSession({
        id: sessionID,
        projectID: "p" as ProjectId,
        workspaceID: workspace as WorkspaceId,
        directory: f.cwd,
        slug: id!,
        title: id!,
        version: "fixture",
      });
      await store.saveSessionEntry({
        id: `${id}-model`,
        sessionID,
        type: "runtime/model_selection",
        time: { created: 1, updated: 1 },
        data: { providerId: "fixture", modelId: "core-model", options: { reasoningLevel: "none" } },
      });
      await store.saveSessionEntry({
        id: `${id}-mode`,
        sessionID,
        type: "runtime/execution_state",
        time: { created: 1, updated: 1 },
        data: { mode, planEnabled: false },
      });
      await store.saveMessage({
        id: messageID,
        sessionID,
        role: "user",
        time: { created: 1 },
        agent: "main",
      });
      await store.savePart({
        id: `${id}-text` as PartId,
        sessionID,
        messageID,
        type: "text",
        text: `${id} history`,
      });
      await store.saveSessionInput({
        id: `${id}-pending`,
        sessionID,
        kind: "sendText",
        delivery: "queue",
        payload: {
          text: "must not execute",
          intent: { sourceCommandId: `${id}-command`, clientId: "old-client" },
        },
      });
    }
    store.close();
    const local = f.start();
    await local.subscribe("conversation/local");
    await assert.rejects(local.rows("remote"), /Session unavailable/);
    const blocked = await local.command(
      local.envelope("sendText", "local", { text: "no elevation" }),
    );
    assert.equal(blocked.status, "rejected");
    assert.equal(f.requests.length, 0);
    const duplicate = await local.command({
      ...local.envelope("sendText", "local", { text: "must not execute" }),
      commandId: "local-command",
    });
    assert.equal(duplicate.status, "failed");
    assert.equal(duplicate.reasonCode, "fault.input.discardedOnRestart");
    await local.command(local.envelope("switchCollaborationMode", "local", { mode: "yolo" }));
    await local.command(local.envelope("sendText", "local", { text: "explicit yolo" }));
    await local.completed("local");
    const remote = f.start("ssh://fixture/workspace");
    await remote.subscribe("conversation/remote");
    await assert.rejects(remote.rows("local"), /Session unavailable/);
    assert.deepEqual(local.schemaErrors, []);
    assert.deepEqual(remote.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("TS compact summary restores its preserved tail without resurrecting summarized history", async () => {
  const f = await fixture({ legacy: true });
  try {
    const store = createSqliteSessionStore({ dbPath: join(f.root, "ts.sqlite") });
    const id = "compact-ts" as SessionId;
    await store.createSession({
      id,
      projectID: "p" as ProjectId,
      workspaceID: f.cwd as WorkspaceId,
      directory: f.cwd,
      slug: "compact",
      title: "compact",
      version: "fixture",
    });
    await store.saveSessionEntry({
      id: "model",
      sessionID: id,
      type: "runtime/model_selection",
      time: { created: 1, updated: 1 },
      data: { providerId: "fixture", modelId: "core-model", options: { reasoningLevel: "none" } },
    });
    await store.saveSessionEntry({
      id: "mode",
      sessionID: id,
      type: "runtime/execution_state",
      time: { created: 1, updated: 1 },
      data: { mode: "yolo", planEnabled: false },
    });
    for (const [mid, text, time] of [
      ["old", "summarized text", 1],
      ["tail", "preserved tail", 2],
    ] as const) {
      await store.saveMessage({
        id: mid as MessageId,
        sessionID: id,
        role: "user",
        time: { created: time },
        agent: "main",
      });
      await store.savePart({
        id: `${mid}-part` as PartId,
        sessionID: id,
        messageID: mid as MessageId,
        type: "text",
        text,
      });
    }
    const summary = "summary" as MessageId;
    await store.saveMessage({
      id: summary,
      sessionID: id,
      role: "assistant",
      parentID: "tail" as MessageId,
      time: { created: 3, completed: 4 },
      agent: "main",
      mode: "yolo",
      summary: true,
      path: { cwd: f.cwd, root: f.cwd },
      cost: 0,
      tokens: { input: 1, output: 1, reasoning: 0, cache: { read: 0, write: 0 } },
    });
    await store.savePart({
      id: "summary-text" as PartId,
      sessionID: id,
      messageID: summary,
      type: "text",
      text: "Summary from TS",
    });
    await store.savePart({
      id: "boundary" as PartId,
      sessionID: id,
      messageID: summary,
      type: "compaction",
      auto: true,
      tail_start_id: "tail" as MessageId,
    });
    store.close();
    const h = f.start();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "continue" }));
    await h.completed(id);
    const request = JSON.stringify(f.requests[0]!.messages);
    assert.match(request, /Summary from TS/);
    assert.match(request, /preserved tail/);
    assert.doesNotMatch(request, /summarized text/);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
