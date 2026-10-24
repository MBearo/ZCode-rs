// rust-cli 与 Node CLI 的会话互操作检查的第七段：`/fork`（docs/specs/rust-cli/session-lifecycle.md §6.5）。
//
// 1. rust-cli 从一条用户输入分出新会话（node_interop `fork`，工作线 F75：复制到这一轮最后一条 assistant、不带
//    checkpoint，F75 之前从 checkpoint 分出并恢复文件）：Node 读出会话行（parent、fork、标题），新会话的历史正好
//    到这一轮的回答，history hydrator 重建的上下文与 rust-cli 的冷读取一致，分叉提示是 Node 的 `rewind_notice`
//    系统提醒、正文是分叉对话的那一段；Node 用自己的写入代码续写这个会话，rust-cli 读回；
// 2. Node 用自己的 `forkConversationFromMessage` 分出 rust-cli 写的会话：rust-cli 读出 fork 会话、Node 的标题、
//    复制的历史与 `fork` 来源的分叉提示。
import { importNode } from "./node-root.mjs";

/** 去掉 `<system-reminder>` 包装（rust-cli 的 attachment 文本自带包装）。 */
const unwrap = (text) =>
  text.replace(/^<system-reminder>\n/, "").replace(/\n<\/system-reminder>$/, "");

/** 人类输入（rust-cli 冷读取的形状）。 */
const isInput = (message) => message.type === "user" && message.origin?.kind === "human";

/** 第一条输入那一轮最后一条 assistant 的 ID（到下一条输入之前）。 */
function firstTurnAnswer(messages) {
  const start = messages.findIndex(isInput);
  const next = messages.findIndex((message, index) => index > start && isInput(message));
  return messages
    .slice(start, next < 0 ? undefined : next)
    .findLast((message) => message.type === "assistant")?.id;
}

/** rust-cli 冷读取里的分叉提示与其余消息。 */
function splitNotice(messages) {
  const notices = messages.filter((m) => m.type === "attachment" && m.source === "fork");
  return { notices, rest: messages.filter((m) => !notices.includes(m)) };
}

export async function checkFork(ctx) {
  const {
    nodeRoot,
    store,
    artifactStore,
    dir,
    rust,
    written,
    nodeRuntime,
    nodeHistory,
    rustText,
    expectEqual,
    persistence,
    createMessageId,
    createPartId,
    createTurnId,
    createMessageHistory,
    hydrateMessageHistoryFromSession,
  } = ctx;
  // 1. rust-cli 从第一条输入分出（第五段会话：第一轮写文件、有工具轮），Node 读与续写。
  const parentID = written.sessions[4];
  const parent = await store.getSession(parentID);
  const parentLoaded = rust("read", dir, parentID);
  const through = firstTurnAnswer(parentLoaded.messages);
  const forked = rust("fork", dir, parentID, through);
  const child = await store.getSession(forked.session);
  expectEqual(
    "Node reads the rust-cli fork row",
    child && [child.parentID, child.taskType, child.title, child.titleSource],
    [parentID, "fork", `Fork of ${parent.title}`, "generated"],
  );
  const history = createMessageHistory();
  await hydrateMessageHistoryFromSession({
    artifactStore,
    history,
    messages: await store.messages({ sessionID: forked.session }),
  });
  const notice = history
    .toRuntimeEntries()
    .find((entry) => entry.kind === "attachment" && String(entry.content).includes("forked"));
  expectEqual(
    "Node sees the fork notice as a rewind notice",
    notice?.metadata?.source,
    "rewind_notice",
  );
  const loaded = rust("read", dir, forked.session);
  const { notices, rest } = splitNotice(loaded.messages);
  expectEqual("rust-cli reads one fork notice", notices.length, 1);
  const end = parentLoaded.messages.findIndex((message) => message.id === through) + 1;
  expectEqual(
    "the fork keeps the conversation through the chosen turn's answer",
    rustText(rest),
    rustText(parentLoaded.messages.slice(0, end)),
  );
  expectEqual(
    "the fork notice is the conversation fork text",
    unwrap(notices[0]?.text ?? "")
      .split("\n")
      .slice(0, 2),
    ["This session was forked from a previous session message.", `parentSessionId: ${parentID}`],
  );
  expectEqual(
    "both sides see the same fork notice",
    unwrap(notices[0]?.text ?? ""),
    notice?.content,
  );
  expectEqual(
    "fork history",
    await nodeHistory(store, artifactStore, child, forked.session),
    rustText(rest),
  );
  const { runtime, selection } = await nodeRuntime(forked.session);
  const trace = { traceId: forked.session, turnId: createTurnId() };
  const userID = createMessageId();
  await persistence.persistUserPrompt.call(
    runtime,
    userID,
    "Node continues the fork",
    undefined,
    trace,
    { sourceCommandId: "node_cmd_fork" },
  );
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
    sessionID: forked.session,
    messageID: assistantID,
    type: "text",
    text: "Node answers in the fork",
    time: { start: created, end: Date.now() },
  });
  const continued = rust("read", dir, forked.session);
  expectEqual(
    "rust-cli reads Node's continuation of the fork",
    continued.messages.slice(-2).map((m) => m.id),
    [userID, assistantID],
  );

  // 2. Node 分出 rust-cli 写的会话（Node 自己的 `forkConversationFromMessage`）。
  const { forkConversationFromMessage } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/session-fork.ts",
  );
  const { copySessionMessagesForFork } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/workspace-checkpoints.ts",
  );
  const { persistAssistantTimelinePartForSession } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/timeline-persistence.ts",
  );
  const sourceID = written.sessions[0];
  const node = (await nodeRuntime(sourceID)).runtime;
  Object.assign(node, {
    rootTraceContext: { traceId: sourceID },
    copySessionMessagesForFork: copySessionMessagesForFork.bind(node),
    persistAssistantTimelinePartForSession: persistAssistantTimelinePartForSession.bind(node),
    persistSyntheticUserNoticeForSession:
      persistence.persistSyntheticUserNoticeForSession.bind(node),
    createEvent: (type, payload) => ({ type, payload }),
    appendEvent: async () => {},
  });
  const sourceRows = await store.messages({ sessionID: sourceID });
  const target = sourceRows.find((m) => m.info.role === "user" && !m.info.synthetic);
  const nodeFork = await forkConversationFromMessage.call(node, {
    targetMessageId: target.info.id,
    traceContext: { traceId: sourceID, turnId: createTurnId() },
  });
  const read = rust("read", dir, nodeFork.forkedSessionId);
  const sourceSession = await store.getSession(sourceID);
  expectEqual(
    "rust-cli reads the Node fork",
    [read.meta.kind, read.meta.parent, read.meta.title],
    ["fork", sourceID, `Fork of ${sourceSession.title}`],
  );
  const nodeSplit = splitNotice(read.messages);
  expectEqual(
    "rust-cli reads Node's fork notice",
    unwrap(nodeSplit.notices[0]?.text ?? "").split("\n")[0],
    "This session was forked from a previous session message.",
  );
  const nodeChild = await store.getSession(nodeFork.forkedSessionId);
  expectEqual(
    "Node fork history",
    await nodeHistory(store, artifactStore, nodeChild, nodeFork.forkedSessionId),
    rustText(nodeSplit.rest),
  );
}
