// rust-cli 与 Node CLI 的会话互操作检查的第十九段：工具结果的位置键（docs/specs/rust-cli/node-store.md §4.6，
// 工作线 F81）。
//
// rust-cli 写一个会话（node_interop `tool-order-write`：一条回复里三个调用，两个 Read 的结果按交出顺序提交、各自后面
// 跟一条附加上下文；Bash 的结果是续写时补的中断结果，与占位、新输入同一批；新输入有回答）：
// 1. Node 读出三个 tool part：状态、内容与 `metadata.rustCli.afterMessageId`（结算时最后一行）；Node 的 history hydrator
//    不读位置键，结果都排在回复之后（node-store.md §7 登记的差异），附加上下文在后面；
// 2. rust-cli 的冷读取把结果插回各自那一行之后（与实时的顺序相同）；
// 3. App：每条消息过 App 的 strict schema，v4 的事件合成不报错；
// 4. Node 用自己的写入代码续写一轮之后，rust-cli 冷读取的前段不变，Node 的新行排在后面。

import { appSchemaRejections } from "./node-interop-history.mjs";

const INTERRUPTED =
  "[Tool call interrupted: the session ended before this call's result was recorded, so its outcome is unknown. Check whether it took effect before relying on it or running it again.]";

/** 条目的简写：工具结果是 `tool:<id>`，附加上下文是 `context:<内容>`，其余是角色与文本。 */
const brief = (items) =>
  items.map((item) => {
    if (item.role === "tool") return `tool:${item.id}`;
    const context = /hook additional context: (.*)\n<\/system-reminder>/.exec(item.text);
    return context ? `context:${context[1]}` : `${item.role}:${item.text}`;
  });

export async function checkToolOrder(ctx) {
  const { nodeRoot, store, artifactStore, dir, rust, nodeHistory, rustText, expectEqual } = ctx;
  const { nodeRuntime, persistence, createMessageId, createPartId, createTurnId } = ctx.node;
  const written = rust("tool-order-write", dir);
  const sessionID = written.session;
  const messages = await store.messages({ sessionID });

  // 1. Node 读出三个 tool part。
  const reply = messages.find((message) => message.info.id === written.reply);
  const parts = Object.fromEntries(
    (reply?.parts ?? [])
      .filter((part) => part.type === "tool")
      .map((part) => [
        part.callID,
        [part.state.status, part.state.output ?? part.state.error, part.metadata?.rustCli?.afterMessageId],
      ]),
  );
  const [second, first] = written.contexts;
  expectEqual("Node reads the tool parts with their position keys", parts, {
    toolu_first: ["completed", "alpha", second],
    toolu_second: ["completed", "beta", written.reply],
    toolu_wait: ["error", INTERRUPTED, first],
  });
  const session = await store.getSession(sessionID);
  const node = await nodeHistory(store, artifactStore, session, sessionID);
  expectEqual("Node puts the results right after the reply", brief(node), [
    "user:Read both files",
    "assistant:Reading.",
    "tool:toolu_first",
    "tool:toolu_second",
    "tool:toolu_wait",
    "context:ctx second",
    "context:ctx first",
    "assistant:No response requested.",
    "user:Again",
    "assistant:Done.",
  ]);

  // 2. rust-cli 的冷读取：结果插回各自那一行之后。
  const loaded = rust("read", dir, sessionID);
  const expected = [
    "user:Read both files",
    "assistant:Reading.",
    "tool:toolu_second",
    "context:ctx second",
    "tool:toolu_first",
    "context:ctx first",
    "tool:toolu_wait",
    "assistant:No response requested.",
    "user:Again",
    "assistant:Done.",
  ];
  expectEqual("rust-cli reads the results back in place", brief(rustText(loaded.messages)), expected);
  expectEqual(
    "the contexts follow their own results",
    loaded.messages
      .filter((message) => message.type === "attachment")
      .map((message) => message.id),
    [second, first],
  );

  // 3. App 的 strict schema。
  expectEqual("the App schema accepts the session", await appSchemaRejections(nodeRoot, store, [sessionID]), []);

  // 4. Node 续写一轮，rust-cli 再冷读取。
  const { runtime, selection } = await nodeRuntime(sessionID);
  const trace = { traceId: sessionID, turnId: createTurnId() };
  const userID = createMessageId();
  await persistence.persistUserPrompt.call(runtime, userID, "Node continues", undefined, trace, {
    sourceCommandId: "node_cmd_tool_order",
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
  const continued = rust("read", dir, sessionID);
  expectEqual("rust-cli keeps the order after Node continued", brief(rustText(continued.messages)), [
    ...expected,
    "user:Node continues",
    "assistant:Node answers",
  ]);
}
