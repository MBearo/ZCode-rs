import assert from "node:assert/strict";
import test from "node:test";
import { access, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { pathToFileURL } from "node:url";
import { fixture } from "./zcode-cli-rust-fixture.js";
import { createSqliteSessionStore } from "../../../apps/zcode-cli/packages/adapters/src/storage/session-store.js";
import type { SessionId, MessageId, ProjectId, WorkspaceId, PartId } from "@zcode/contracts";
import { zcodeSessionStateSnapshotSchema } from "@zcode/shared";

// Node 与 Rust 共用同一个会话库（spec rust-m11-node-storage §11 场景 2、3）：
// Node 写入的会话由 Rust 直接打开并继续，Rust 写入的内容 Node 仍能读取。
test("Rust opens a Node session with its identity, attachments, tools and interrupted outcomes; Node and Rust then continue it in turn", async () => {
  const f = await fixture();
  try {
    const store = createSqliteSessionStore({ dbPath: f.db });
    const id = "ts-session" as SessionId;
    const user = "ts-user" as MessageId;
    const assistant = "ts-assistant" as MessageId;
    await store.createSession({
      id,
      projectID: "fixture-project" as ProjectId,
      workspaceID: f.cwd as WorkspaceId,
      directory: f.cwd,
      slug: "fixture",
      title: "Old Node task",
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
    const h = f.start();
    await h.subscribe(`conversation/${id}`);
    const snapshot = await h.client.request(
      "session/read",
      { sessionId: id },
      zcodeSessionStateSnapshotSchema,
    );
    assert.equal(snapshot.session.title, "Old Node task");
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
    // Node hydrator：没有预览的本地文本附件只留占位，未结束的工具结果为中断。
    assert.match(JSON.stringify(f.requests[0]!.messages), /\[Attached text\/plain: old\.txt\]/);
    assert.match(
      JSON.stringify(f.requests[0]!.messages),
      /Tool execution was interrupted before resume/,
    );
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
    const reader = createSqliteSessionStore({ dbPath: f.db });
    const texts = (await reader.messages({ sessionID: id }))
      .flatMap((m) => m.parts)
      .flatMap((p) => (p.type === "text" ? [p.text] : []));
    assert.deepEqual(texts, ["old question", "old answer", "continue old task", "你好 Rust"]);
    // Node 再续一轮（与 Node 运行时一样按序追加 user 与 assistant），Rust 重启后接着继续。
    const nodeUser = "node-user-2" as MessageId;
    await reader.saveMessage({
      id: nodeUser,
      sessionID: id,
      role: "user",
      time: { created: Date.now() },
      agent: "main",
    });
    await reader.savePart({
      id: "node-user-2-text" as PartId,
      sessionID: id,
      messageID: nodeUser,
      type: "text",
      text: "node follow up",
    });
    await reader.saveMessage({
      id: "node-assistant-2" as MessageId,
      sessionID: id,
      role: "assistant",
      parentID: nodeUser,
      time: { created: Date.now(), completed: Date.now() },
      agent: "main",
      mode: "yolo",
      path: { cwd: f.cwd, root: f.cwd },
      cost: 0,
      tokens: { input: 1, output: 1, reasoning: 0, cache: { read: 0, write: 0 } },
    });
    await reader.savePart({
      id: "node-assistant-2-text" as PartId,
      sessionID: id,
      messageID: "node-assistant-2" as MessageId,
      type: "text",
      text: "node answer",
    });
    reader.close();
    const h3 = f.start();
    await h3.subscribe(`conversation/${id}`);
    await h3.command(h3.envelope("sendText", id, { text: "rust again" }));
    await h3.completed(id);
    const conversation = f.requests
      .at(-1)!
      .messages.filter((m: any) => m.role !== "system" && typeof m.content === "string")
      .map((m: any) => m.content)
      .filter((c: string) => !c.startsWith("<system-reminder>"));
    assert.deepEqual(conversation.slice(-5), [
      "continue old task",
      "你好 Rust",
      "node follow up",
      "node answer",
      "rust again",
    ]);
    const after = createSqliteSessionStore({ dbPath: f.db });
    const sequence = (await after.messages({ sessionID: id })).map((m) => m.info.role);
    after.close();
    assert.deepEqual(sequence.slice(-2), ["user", "assistant"]);
    assert.deepEqual([...h.schemaErrors, ...h2.schemaErrors, ...h3.schemaErrors], []);
  } finally {
    await f.close();
  }
});

test("Node sessions keep workspace identity and build approvals; admitted inputs are discarded on resume", async () => {
  const f = await fixture();
  try {
    const store = createSqliteSessionStore({ dbPath: f.db });
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
    // Node 的 build 会话按 build 执行：写文件先询问，不会被静默提升为 yolo。
    let after = local.messages.length;
    assert.equal(
      (await local.command(local.envelope("sendText", "local", { text: "write" }))).status,
      "accepted",
    );
    const asked = await local.wait(
      (m) =>
        m.params?.topic === "conversation/local" &&
        m.params.frame?.payload?.deltas?.some((d: any) => d.patch?.pendingInteractions?.length),
      after,
    );
    const prompt = asked.params.frame.payload.deltas.find(
      (d: any) => d.patch?.pendingInteractions?.length,
    ).patch.pendingInteractions[0];
    assert.equal(prompt.payload.toolName, "Write");
    await local.command(
      local.envelope("resolveInteraction", "local", {
        interactionId: prompt.interactionId,
        answer: { optionId: "deny" },
      }),
    );
    await local.completed("local", after);
    await assert.rejects(access(join(f.cwd, "result.txt")));
    const duplicate = await local.command({
      ...local.envelope("sendText", "local", { text: "must not execute" }),
      commandId: "local-command",
    });
    assert.equal(duplicate.status, "failed");
    // Node `discardAdmittedOnLoad`：未开始的输入恢复时结算为 discarded/session_resumed。
    assert.equal(duplicate.reasonCode, "fault.command.inputDiscardedOnRestart");
    await local.command(local.envelope("switchCollaborationMode", "local", { mode: "yolo" }));
    after = local.messages.length;
    await local.command(local.envelope("sendText", "local", { text: "explicit yolo" }));
    await local.completed("local", after);
    const remote = f.start("ssh://fixture/workspace");
    await remote.subscribe("conversation/remote");
    await assert.rejects(remote.rows("local"), /Session unavailable/);
    assert.deepEqual(local.schemaErrors, []);
    assert.deepEqual(remote.schemaErrors, []);
  } finally {
    await f.close();
  }
});

test("Node compact summary restores its preserved tail without resurrecting summarized history", async () => {
  const f = await fixture();
  try {
    const store = createSqliteSessionStore({ dbPath: f.db });
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
      text: "Summary from Node",
    });
    await store.savePart({
      id: "boundary" as PartId,
      sessionID: id,
      messageID: summary,
      type: "compaction",
      auto: true,
      compactBoundary: {
        trigger: "auto",
        preCompactTokenCount: 10,
        summarizedMessageCount: 1,
        summaryMessageIds: [summary],
        traceId: "trace",
        preservedSegment: {
          headMessageId: "tail",
          anchorMessageId: summary,
          tailMessageId: "tail",
        },
      },
    } as never);
    store.close();
    const h = f.start();
    await h.subscribe(`conversation/${id}`);
    await h.command(h.envelope("sendText", id, { text: "continue" }));
    await h.completed(id);
    const request = JSON.stringify(f.requests[0]!.messages);
    assert.match(request, /Summary from Node/);
    assert.match(request, /preserved tail/);
    assert.doesNotMatch(request, /summarized text/);
    assert.deepEqual(h.schemaErrors, []);
  } finally {
    await f.close();
  }
});
