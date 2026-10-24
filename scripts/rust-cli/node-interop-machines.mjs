// rust-cli 与 Node CLI 的会话互操作检查的第二十四段：远端机器上的工具调用（docs/specs/rust-cli/remote-exec.md §20.9，R3）。
//
// rust-cli 写一个会话（node_interop `machines-write`：机器清单提醒；一条回复里 Bash 与 Write 在 dev-a 上跑、另一个
// Write 在本机；本机的 checkpoint 是 Node 的 `runtime/workspace_checkpoint`，远端的是 `rust-cli/remote_checkpoint`）：
// 1. Node 读出 tool part：输入里的 `machine` 原样在（普通的工具输入）；提醒在 Node 的 history 里（未知来源，原文给模型）；
// 2. App：每条消息过 App 的 strict schema；
// 3. Node 只看到本机的那个 checkpoint（按类型查询）；本工程的记录在，rust-cli 冷读取两个都读回，远端的带机器名；
// 4. Node 用自己的写入代码续写一轮之后，rust-cli 冷读取的前段与 checkpoint 不变。

import { appSchemaRejections } from "./node-interop-history.mjs";

export async function checkMachines(ctx) {
  const { nodeRoot, store, artifactStore, dir, rust, nodeHistory, rustText, expectEqual } = ctx;
  const { nodeRuntime, persistence, createMessageId, createPartId, createTurnId } = ctx.node;
  const written = rust("machines-write", dir);
  const sessionID = written.session;
  const messages = await store.messages({ sessionID });

  // 1. Node 读出 tool part 与它们的输入。
  const reply = messages.find((message) => message.info.id === written.reply);
  const inputs = Object.fromEntries(
    (reply?.parts ?? [])
      .filter((part) => part.type === "tool")
      .map((part) => [part.callID, [part.state.status, part.state.input?.machine ?? null]]),
  );
  expectEqual("Node reads the machine in the tool inputs", inputs, {
    toolu_bash: ["completed", "dev-a"],
    toolu_remote: ["completed", "dev-a"],
    toolu_local: ["completed", null],
  });
  const session = await store.getSession(sessionID);
  const node = await nodeHistory(store, artifactStore, session, sessionID);
  const nodeText = JSON.stringify(node);
  expectEqual(
    "Node sends the machine list reminder to the model",
    nodeText.includes("Remote machines connected to this session."),
    true,
  );
  const rustBefore = rust("read", dir, sessionID);
  expectEqual("Node and rust-cli read the same history", node, rustText(rustBefore.messages));

  // 2. App 的 strict schema。
  expectEqual(
    "the App schema accepts the session",
    await appSchemaRejections(nodeRoot, store, [sessionID]),
    [],
  );

  // 3. checkpoint：Node 只看到本机的；rust-cli 两个都读回。
  const nodeCheckpoints = await store.sessionEntries({
    sessionID,
    type: "runtime/workspace_checkpoint",
  });
  expectEqual(
    "Node sees only the local checkpoint",
    nodeCheckpoints.map((entry) => entry.data.payload.checkpointId),
    ["checkpoint_local"],
  );
  const remoteEntries = await store.sessionEntries({
    sessionID,
    type: "rust-cli/remote_checkpoint",
  });
  expectEqual(
    "the remote checkpoint is a rust-cli entry",
    remoteEntries.map((entry) => [entry.data.payload.checkpointId, entry.data.rustCli.machine]),
    [["checkpoint_remote", "dev-a"]],
  );
  const checkpoints = (loaded) => loaded.checkpoints.map((c) => [c.id, c.machine ?? null]);
  expectEqual("rust-cli reads both checkpoints back", checkpoints(rustBefore), [
    ["checkpoint_local", null],
    ["checkpoint_remote", "dev-a"],
  ]);

  // 4. Node 续写一轮，rust-cli 再冷读取。
  const { runtime, selection } = await nodeRuntime(sessionID);
  const trace = { traceId: sessionID, turnId: createTurnId() };
  const userID = createMessageId();
  await persistence.persistUserPrompt.call(runtime, userID, "Node continues", undefined, trace, {
    sourceCommandId: "node_cmd_machines",
  });
  const assistantID = createMessageId();
  const created = Date.now();
  await persistence.persistAssistantMessage.call(
    runtime,
    assistantID,
    userID,
    created,
    { completed: Date.now(), finish: "stop" },
    trace,
    { providerId: selection.providerId, modelId: selection.modelId },
  );
  await runtime.persistPart({
    id: createPartId(),
    sessionID,
    messageID: assistantID,
    type: "text",
    text: "Node answers",
    time: { start: created, end: Date.now() },
  });
  const after = rust("read", dir, sessionID);
  const before = rustText(rustBefore.messages);
  expectEqual(
    "rust-cli keeps the history after Node continued",
    rustText(after.messages).slice(0, before.length),
    before,
  );
  expectEqual(
    "rust-cli keeps both checkpoints after Node continued",
    checkpoints(after),
    checkpoints(rustBefore),
  );
}
