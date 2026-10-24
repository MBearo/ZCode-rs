// rust-cli 与 Node CLI 的会话互操作检查的第二十一段：Node 写的、中途被杀的会话由 rust-cli 续写（docs/specs/rust-cli/
// node-store.md §4.6、§6 第 21 步，session-lifecycle.md §6.3.2，工作线 F86、F88）。要模型替身与临时 HOME，作为第十一段
// （node-interop-runtime.mjs）的一个场景在它的子进程里跑。
//
// 1. Node 的真实运行时建会话、跑一轮；再用 Node 自己的写入代码写一个被杀的步骤：真实输入、没有完成的 assistant 与
//    `step-start`、文字、三个 tool part（声明顺序第一个 running、第二个已结算成 completed、第三个 pending，都没有
//    `metadata.rustCli`），没有 `step-finish`（Node 被杀时的样子）；
// 2. 真实的 `zcode -p -r` 续写一轮：请求里三个调用之后，按声明顺序是 Node 的中断结果、Node 写的结果、Node 的中断结果
//    （Node 的文字，`is_error`），再是续写占位与新输入；库里三个 tool part 与续写前相同，没有 `rust-cli/operation_result`；
// 3. 再续写一轮：请求的前段是上一轮的请求加上回复（补的结果在原处）；tool part 仍不变；
// 4. Node 读：history hydrator 补的中断结果与 rust-cli 的冷读取逐条相同；App 的 strict schema 通过；
// 5. Node 再续写一轮：Node 的请求里这三个调用的结果（顺序、文字、`is_error`）与 rust-cli 第一轮请求里的相同；rust-cli
//    冷读取的前段不变。
import { join } from "node:path";
import { appSchemaRejections, rustText } from "./node-interop-history.mjs";
import { conversation, replyItems } from "./node-interop-runtime-harness.mjs";
import { importNode } from "./node-root.mjs";

/** Node 冷恢复给未结调用补的中断结果（`session-history-hydrator.ts` 的 `INTERRUPTED_TOOL_RESULT`）。 */
const NODE_INTERRUPTED = "[Tool execution was interrupted before resume]";
/** 第二个调用由 Node 结算的输出。 */
const READ_OUTPUT = "1\tkilled notes";

/** Node 写入代码要的 runtime 成员（`AgentRuntimeInternal` 的子集，同 node-interop-check.mjs 的 `nodeRuntime`）。 */
async function nodeWriter(ctx, sessionID) {
  const { nodeRoot, store, layout } = ctx;
  const persistence = await importNode(nodeRoot, "core/src/runtime/methods/message-persistence.ts");
  const { persistPendingToolPart } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/tool-part-persistence.ts",
  );
  const { completedToolPartMetadata } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/tool-part-metadata.ts",
  );
  const ids = await importNode(nodeRoot, "contracts/src/interfaces/shared.ts");
  const { readSessionModelSelection } = await importNode(
    nodeRoot,
    "bootstrap/src/app/session-store.ts",
  );
  const selection = await readSessionModelSelection(store, sessionID);
  const entries = await store.sessionEntries({ sessionID, type: "runtime/execution_state" });
  const execution = entries.at(-1)?.data ?? { mode: "yolo", planEnabled: false };
  const runtime = {
    sessionId: sessionID,
    sessionStore: store,
    config: { mode: execution.mode },
    getTools: () => [],
    getSessionModelSelection: () => selection,
    getPlanEnabled: () => execution.planEnabled,
    workingDirectory: layout.work,
    workspaceRoot: layout.work,
  };
  runtime.persistMessage = persistence.persistMessage.bind(runtime);
  runtime.persistPart = persistence.persistPart.bind(runtime);
  return {
    runtime,
    persistence,
    persistPendingToolPart,
    completedToolPartMetadata,
    ids,
    selection,
  };
}

/** Node 被杀前写下的一步（Node 的写入代码，顺序同 `turn-model-step.ts` 与 `turn-tools.ts`）。 */
async function writeKilledStep(ctx, sessionID) {
  const {
    runtime,
    persistence,
    persistPendingToolPart,
    completedToolPartMetadata,
    ids,
    selection,
  } = await nodeWriter(ctx, sessionID);
  const trace = { traceId: sessionID, turnId: ids.createTurnId() };
  const askID = ids.createMessageId();
  await persistence.persistUserPrompt.call(runtime, askID, "K2 three tools", undefined, trace, {
    sourceCommandId: "node_cmd_killed",
  });
  const stepID = ids.createMessageId();
  const created = Date.now();
  const model = { providerId: selection.providerId, modelId: selection.modelId };
  // 步骤开始时的 assistant 没有 `completed` 与 `finish`，`step-finish` 要等工具都结算之后才写。
  await persistence.persistAssistantMessage.call(
    runtime,
    stepID,
    askID,
    created,
    undefined,
    trace,
    model,
  );
  const owner = { sessionID, messageID: stepID };
  await runtime.persistPart({ id: ids.createPartId(), ...owner, type: "step-start" });
  await runtime.persistPart({
    id: ids.createPartId(),
    ...owner,
    type: "text",
    text: "Working.",
    time: { start: created, end: Date.now() },
  });
  const calls = [
    { id: "call_killed_bash", name: "Bash", input: { command: "sleep 30" } },
    {
      id: "call_killed_read",
      name: "Read",
      input: { file_path: join(ctx.layout.work, "notes.txt") },
    },
    { id: "call_killed_echo", name: "Bash", input: { command: "echo later" } },
  ];
  const partIDs = [];
  for (const [declarationIndex, toolCall] of calls.entries()) {
    const partID = ids.createPartId();
    partIDs.push(partID);
    await persistPendingToolPart(runtime, {
      assistantMessageId: stepID,
      declarationIndex,
      input: toolCall.input,
      partID,
      toolCall,
      traceContext: trace,
      model,
    });
  }
  const tool = (index, state) => ({
    id: partIDs[index],
    ...owner,
    type: "tool",
    callID: calls[index].id,
    declarationIndex: index,
    tool: calls[index].name,
    metadata: {},
    state: { input: calls[index].input, title: calls[index].name, ...state },
  });
  // 第一个调用开工（`onBatchStart` 的 running 写法）；第二个（只读工具，可能先落盘，`tool-part-order.ts`）已结算成
  // completed（`settleToolResults` 的写法）；第三个还没开工。之后进程被杀。
  const started = Date.now();
  await runtime.persistPart(tool(0, { status: "running", metadata: {}, time: { start: started } }));
  await runtime.persistPart(
    tool(1, {
      status: "completed",
      output: READ_OUTPUT,
      metadata: completedToolPartMetadata({}),
      time: { start: started, end: Date.now() },
    }),
  );
  return { stepID, calls };
}

/** 这一轮新发的主请求。 */
function mainRequests(ctx, cursor) {
  return ctx.model.records.slice(cursor).filter((record) => record.kind === "main");
}

/** 请求里这几个调用的结果，按出现的顺序。 */
function resultsOf(items, calls) {
  return items
    .filter((item) => item.tool_result && calls.some(({ id }) => id === item.tool_result.id))
    .map((item) => item.tool_result);
}

export async function nodeKilled(ctx) {
  const { node, rust, store, artifactStore, check, nodeRoot } = ctx;
  const created = await node.turn(null, "K1 hello");
  const sessionID = created.session;
  const { stepID, calls } = await writeKilledStep(ctx, sessionID);
  const toolParts = async () =>
    (await store.messages({ sessionID }))
      .find((message) => message.info.id === stepID)
      ?.parts.filter((part) => part.type === "tool");
  const before = await toolParts();
  check(
    "killed: Node wrote a running, a completed and a pending tool part without a rust-cli mark",
    before?.map((part) => [part.callID, part.state.status, part.metadata?.rustCli ?? null]),
    [
      ["call_killed_bash", "running", null],
      ["call_killed_read", "completed", null],
      ["call_killed_echo", "pending", null],
    ],
  );

  // 2. rust-cli 续写一轮：Node 的文字，按声明顺序夹在 Node 写的结果中间。
  let cursor = ctx.model.records.length;
  await rust.turn(sessionID, "K3 again");
  const first = mainRequests(ctx, cursor);
  check("killed: rust-cli K3 sent one main request", first.length, 1);
  const items = conversation(first[0]?.parsed);
  const uses = calls.map(({ id, name, input }) => ({
    role: "assistant",
    tool_use: { id, name, input },
  }));
  const expected = [
    { id: calls[0].id, error: true, text: NODE_INTERRUPTED },
    { id: calls[1].id, error: false, text: READ_OUTPUT },
    { id: calls[2].id, error: true, text: NODE_INTERRUPTED },
  ];
  check("killed: the request carries Node's interrupted results in place", items.slice(-10), [
    { role: "user", text: "K2 three tools" },
    { role: "assistant", text: "Working." },
    ...uses,
    ...expected.map((tool_result) => ({ role: "user", tool_result })),
    { role: "assistant", text: "No response requested." },
    { role: "user", text: "K3 again" },
  ]);
  check(
    "killed: the Node tool parts are unchanged after rust-cli continued",
    await toolParts(),
    before,
  );
  check(
    "killed: no operation results were recorded",
    await store.sessionEntries({ sessionID, type: "rust-cli/operation_result" }),
    [],
  );

  // 3. 再续写一轮：补的结果在原处，请求的前段与上一轮相同。
  cursor = ctx.model.records.length;
  await rust.turn(sessionID, "K4 more");
  const second = mainRequests(ctx, cursor);
  check("killed: rust-cli K4 sent one main request", second.length, 1);
  check("killed: the next request repeats the history", conversation(second[0]?.parsed), [
    ...items,
    ...replyItems(first[0]?.reply),
    { role: "user", text: "K4 more" },
  ]);
  check("killed: the Node tool parts stay unchanged", await toolParts(), before);

  // 4. Node 读：自己补的中断结果与 rust-cli 的冷读取逐条相同（文字也相同）；App 的 strict schema 通过。
  const session = await store.getSession(sessionID);
  const rustBefore = rustText((await rust.read(sessionID)).messages);
  check(
    "killed: cold history (Node hydrator, rust-cli load)",
    await ctx.nodeHistory(store, artifactStore, session, sessionID),
    rustBefore,
  );
  check(
    "killed: every message passes the App protocol schema",
    await appSchemaRejections(nodeRoot, store, [sessionID]),
    [],
  );

  // 5. Node 再续写一轮：Node 自己补的结果（顺序、文字、`is_error`）与 rust-cli 第一轮请求里的相同；rust-cli 冷读取的
  //    前段不变。
  cursor = ctx.model.records.length;
  await node.turn(sessionID, "K5 node");
  const nodeRequest = mainRequests(ctx, cursor)[0];
  check(
    "killed: rust-cli sends the results Node sends (text and position)",
    resultsOf(items, calls),
    resultsOf(conversation(nodeRequest?.parsed), calls),
  );
  check(
    "killed: Node still supplies its own interrupted results",
    resultsOf(conversation(nodeRequest?.parsed), calls),
    expected,
  );
  const rustAfter = rustText((await rust.read(sessionID)).messages);
  check(
    "killed: rust-cli keeps the history after Node continued",
    rustAfter.slice(0, rustBefore.length),
    rustBefore,
  );
}
