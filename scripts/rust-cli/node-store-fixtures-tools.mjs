#!/usr/bin/env node
// 生成 node-store 的 Node 工具会话夹具（docs/specs/rust-cli/node-store.md §4.6、§6）。
//
// 用 Node 运行时真实的写入函数在固定时钟与固定 uuid 下写一个带工具调用的回合：
//   persistUserPrompt → persistAssistantMessage / persistPart（步骤开始）→ persistPendingToolPart
//   → settleToolResults（运行时结算工具结果的唯一路径）→ persistToolModelStepFinish → 回合结束事件
//   （appendEvent → persistDurableSessionEvent 写 lastOutcome）。
// 工具结果是 Node 执行器的 ToolExecutionResult：成功结果按执行器的字段给出，失败结果由执行器的
// createErrorResult(createToolHandlerFailureError(…)) 生成。
// 把各表的行写进 packages/rust-cli/crates/node-store/fixtures/node-records-tools.json；
// crates/node-store/src/records_tools_tests.rs 用 NodeStore 按 query 主循环的提交顺序提交同一组事实，
// 逐字节比对。两边的场景与时间线必须一致：改这里时同步改 records_tools_tests.rs。
//
// 用法：node scripts/rust-cli/node-store-fixtures-tools.mjs [--node-root <装好依赖的检出>]
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ensureTypeScript, importNode, nodeRootFromArgs, repoRoot } from "./node-root.mjs";

const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
ensureTypeScript(import.meta.filename, nodeRoot, args);

// 固定时钟：Date.now 与无参的 new Date() 都读 `now`（事件时间、执行器结果的起止时间用 new Date()）。
let now = 0;
const RealDate = Date;
class FixedDate extends RealDate {
  constructor(...values) {
    if (values.length === 0) super(now);
    else super(...values);
  }

  static now() {
    return now;
  }
}
globalThis.Date = FixedDate;

// 固定 uuid：与 records_tools_tests.rs 的 FixedSource 相同（第 n 个是 …-<n 补足 12 位>）。
// `expect` 排好写进库的 id，用不完或不够都报错。事件 id（createSessionEvent，事件只进内存事件库、不进
// 这四张表）另用一个顺序的占位值，不占队列。
const uuid = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
let queue = [];
let eventIds = 0;
Object.defineProperty(globalThis.crypto, "randomUUID", {
  configurable: true,
  value: () => {
    if (new Error().stack?.includes("createSessionEvent")) {
      eventIds += 1;
      return `eeeeeeee-eeee-4eee-8eee-${String(eventIds).padStart(12, "0")}`;
    }
    if (!queue.length) throw new Error("unexpected randomUUID call");
    return queue.shift();
  },
});
const expect = async (ids, run) => {
  queue = ids.map(uuid);
  await run();
  if (queue.length) throw new Error(`unused uuids: ${queue.join(", ")}`);
};

const { DatabaseSync } = await import("node:sqlite");
const { SqliteSessionStore } = await importNode(
  nodeRoot,
  "adapters/src/storage/session-store/sqlite-session-store.ts",
);
const { createNodeToolArtifactStore, createInMemorySessionEventStore } = await importNode(
  nodeRoot,
  "adapters/src/storage/index.ts",
);
const persistence = await importNode(nodeRoot, "core/src/runtime/methods/message-persistence.ts");
const { persistPendingToolPart } = await importNode(
  nodeRoot,
  "core/src/runtime/methods/tool-part-persistence.ts",
);
const { settleToolResults } = await importNode(
  nodeRoot,
  "core/src/runtime/methods/tool-result-settlement.ts",
);
const { persistToolModelStepFinish } = await importNode(
  nodeRoot,
  "core/src/runtime/methods/turn-step-finish.ts",
);
const events = await importNode(nodeRoot, "core/src/runtime/methods/events.ts");
const { emitFileMutationCheckpoint } = await importNode(
  nodeRoot,
  "core/src/runtime/methods/tools.ts",
);
const { TurnMachineImpl } = await importNode(nodeRoot, "core/src/agent/turn-machine.ts");
const { createErrorResult, createToolHandlerFailureError } = await importNode(
  nodeRoot,
  "core/src/tool/executor/errors.ts",
);
const { SessionEventType } = await importNode(nodeRoot, "core/src/runtime/deps.ts");
const { buildExecutionStateEntry } = await importNode(
  nodeRoot,
  "core/src/runtime/execution-state.ts",
);
const { toTokenUsageInfo, toRecordInput } = await importNode(
  nodeRoot,
  "core/src/runtime/helpers/tool-result.ts",
);
const { titleFromInput, slugify, projectIdFromDirectory } = await importNode(
  nodeRoot,
  "core/src/runtime/helpers/project.ts",
);

const SESSION = "sess_tools";
const DIRECTORY = "/w/repo";
const msg = (ms, suffix) => `msg_${ms.toString(36)}_${suffix}`;
const part = (n) => `part_${now.toString(36)}_${uuid(n)}`;
const PNG = "iVBORw0KGgo=";

const dir = await mkdtemp(join(tmpdir(), "rust-cli-node-records-tools-"));
try {
  const dbPath = join(dir, "db.sqlite");
  const store = await SqliteSessionStore.openStartup({ dbPath });
  const selection = { providerId: "p", modelId: "m", options: { reasoningLevel: "high" } };
  const model = { providerId: "p", modelId: "m" };
  const execution = { mode: "build", planEnabled: false };
  // Node 写入函数要的 runtime 成员（`AgentRuntimeInternal` 的子集）。持久化相关的成员都是 Node 的真实实现：
  // 会话库、artifact store（临时目录）、persistMessage / persistPart / persistAssistantMessage、
  // createEvent / appendEvent（事件库是 Node 的内存实现）、emitFileMutationCheckpoint。
  // 其余只影响内存状态的成员给最小实现：工具注册表为空（没有 MCP 展示与工具元数据），模型历史不记录，
  // 事件不发给订阅方。
  const runtime = {
    sessionId: SESSION,
    sessionStore: store,
    artifactStore: createNodeToolArtifactStore({
      imageCacheRootDir: join(dir, "cli/image-cache"),
      pdfCacheRootDir: join(dir, "cli/pdf-cache"),
      rootDir: join(dir, "cli/artifacts"),
      videoCacheRootDir: join(dir, "cli/video-cache"),
    }),
    eventStore: createInMemorySessionEventStore(),
    config: {},
    registry: { get: () => undefined, getMetadata: () => undefined },
    messageHistory: { addEntries: () => {} },
    notifyEventSinks: async () => {},
    getTools: () => [],
    getSessionModelSelection: () => selection,
    getPlanEnabled: () => execution.planEnabled,
    workingDirectory: DIRECTORY,
    workspaceRoot: DIRECTORY,
  };
  runtime.persistMessage = persistence.persistMessage.bind(runtime);
  runtime.persistPart = persistence.persistPart.bind(runtime);
  runtime.persistAssistantMessage = persistence.persistAssistantMessage.bind(runtime);
  runtime.createEvent = events.createEvent.bind(runtime);
  runtime.appendEvent = events.appendEvent.bind(runtime);
  runtime.emitFileMutationCheckpoint = emitFileMutationCheckpoint.bind(runtime);

  const turn = `turn_${uuid(1)}`;
  const trace = { traceId: SESSION, turnId: turn };
  const u1 = msg(1002, "u1");
  const input = "Show logo.png and a.txt";

  // 一个模型步骤开始（Node turn-model-step）：未完成的 assistant 与 step-start。node-store.md §7
  // “assistant 一次写入”：本工程在流结束时才写，所以这里也在流结束的时刻写。
  const stepStart = async (id, created, stepStartPart) => {
    await runtime.persistAssistantMessage(id, u1, created, undefined, trace, model);
    await runtime.persistPart({
      id: part(stepStartPart),
      sessionID: SESSION,
      messageID: id,
      type: "step-start",
    });
  };
  // 本步骤的工具调用：每个一个 pending part（turn-tools 的 persistPendingToolPart，input 经 toRecordInput），
  // 返回 settleToolResults 要的 toolParts / toolCallById。startedAt 是 Node 写 running 时的时间，结算时不用。
  const pendingTools = async (assistantMessageId, calls) => {
    const toolParts = new Map();
    const toolCallById = new Map();
    for (const [declarationIndex, { call, partUuid }] of calls.entries()) {
      const partID = part(partUuid);
      const recordInput = toRecordInput(call.input);
      toolParts.set(call.id, { declarationIndex, partID, input: recordInput, startedAt: now });
      toolCallById.set(call.id, call);
      await persistPendingToolPart(runtime, {
        assistantMessageId,
        declarationIndex,
        input: recordInput,
        partID,
        toolCall: call,
        traceContext: trace,
        model,
      });
    }
    return { toolParts, toolCallById };
  };
  // 执行到“工具执行中”的回合状态机（settleToolResults 要在这个阶段 completeTool / aggregateResults）。
  const executing = (calls) => {
    const machine = TurnMachineImpl.create(SESSION, 1, input, SESSION, turn);
    let state = new TurnMachineImpl(machine.start());
    state = new TurnMachineImpl(state.startModelRequest("m", []));
    state = new TurnMachineImpl(state.receiveModelResponse(""));
    state = new TurnMachineImpl(
      state.scheduleTools(calls, { parallelGroups: [calls.map((c) => c.id)] }),
    );
    return new TurnMachineImpl(state.startToolExecution());
  };
  const loopState = (calls) => ({
    turnId: turn,
    userMessageId: u1,
    currentUserMessageId: u1,
    model,
    events: [],
    turnRequestState: { entries: [] },
    turnMachine: executing(calls),
    turnAbortSignal: undefined,
    activeTurn: undefined,
  });
  // 工具执行完：结果按声明顺序整批结算（turn-tools），再写 step-finish 并重写 assistant（completed）。
  const settle = async (state, assistantMessageId, created, pending, results, step) => {
    await settleToolResults.call(runtime, state, {
      assistantMessageId,
      modelTraceContext: trace,
      results,
      ...pending,
      streamedResultsById: new Map(),
    });
    await persistToolModelStepFinish(runtime, state, {
      assistantCreatedAt: created,
      assistantMessageId,
      modelTraceContext: trace,
      result: { finishReason: step.finish, usage: step.usage },
    });
  };

  // create(meta)：title 为空 → default；permission 是 Node 的缺省 build。
  now = 1000;
  await store.createSession({
    id: SESSION,
    projectID: projectIdFromDirectory(DIRECTORY),
    traceID: SESSION,
    slug: slugify(SESSION),
    directory: DIRECTORY,
    path: DIRECTORY,
    title: "",
    titleSource: "default",
    version: "0.1.0",
    permission: { mode: "build" },
    time: { created: 1000 },
  });

  // commit 1：ModelSelected、ModeChanged(default)。
  now = 1001;
  await store.saveSessionEntry({
    id: `${SESSION}:runtime-model-selection`,
    sessionID: SESSION,
    type: "runtime/model_selection",
    touchSession: false,
    time: { created: now, updated: now },
    data: selection,
  });
  await store.updateSession({ id: SESSION, permission: { mode: "build" } });
  await store.saveSessionEntry(buildExecutionStateEntry(SESSION, execution));

  // commit 2（命令 cmd_1）：user 文本。turn 用第 1 个 uuid，text part 用第 2 个。
  now = 1002;
  await expect([2], async () => {
    await persistence.persistUserPrompt.call(runtime, u1, input, undefined, trace, {
      sourceCommandId: "cmd_1",
    });
  });
  await store.updateSession({
    id: SESSION,
    title: titleFromInput(input),
    titleSource: "first_input",
    expectedTitleSources: ["default"],
  });

  // 第一步：文本 + 两个并发的工具调用。A（Read 图片）先声明、最后结束；B（Bash）后声明、先结束。
  // 两个工具在流式响应中开工（1004、1005，本工程此时提交 OperationIntent，是 rust-cli/* entry），流在 1010 结束。
  const a1 = msg(1003, "a1");
  const callA = { id: "toolu_a", name: "Read", input: { file_path: "/w/repo/logo.png" } };
  const callB = {
    id: "toolu_b",
    name: "Bash",
    input: { command: "cat a.txt", description: "Show a.txt" },
  };
  const step1 = {
    finish: "tool-calls",
    usage: {
      inputTokens: 15,
      outputTokens: 5,
      totalTokens: 20,
      cacheReadTokens: 2,
      cacheWriteTokens: 3,
    },
  };
  now = 1010;
  await stepStart(a1, 1003, 3);
  await runtime.persistPart({
    id: part(4),
    sessionID: SESSION,
    messageID: a1,
    type: "text",
    text: "I'll look at both.",
    time: { start: 1003, end: now },
  });
  const pending1 = await pendingTools(a1, [
    { call: callA, partUuid: 5 },
    { call: callB, partUuid: 6 },
  ]);
  const state1 = loopState([callA, callB]);
  // Node 在批次开始时把两个 part 写成 running；结算时整行替换（time_created 不变），最终的行与不写相同，略过。
  // B 在 1020 结束，A 在 1030 结束；Node 等整批结束（1030）才按声明顺序结算。
  const resultA = {
    toolCallId: callA.id,
    toolName: callA.name,
    success: true,
    output: "logo.png",
    modelContent: [
      { type: "text", text: "Image: logo.png" },
      { type: "image", mediaType: "image/png", dataUrl: `data:image/png;base64,${PNG}` },
    ],
    durationMs: 26,
    startedAt: new Date(1004),
    completedAt: new Date(1030),
  };
  const resultB = {
    toolCallId: callB.id,
    toolName: callB.name,
    success: true,
    output: "hello from a.txt",
    modelContent: "hello from a.txt",
    durationMs: 15,
    startedAt: new Date(1005),
    completedAt: new Date(1020),
  };
  now = 1030;
  // A 的媒体：Node 先写 artifact（id 里的 uuid 只在 Node 一侧，给 901），再建 file part（第 8 个）；
  // step-finish 是第 7 个（本工程在 1010 写 assistant 时就分配了）。
  await expect([901, 8, 7], () => settle(state1, a1, 1003, pending1, [resultA, resultB], step1));

  // 第二步：推理 + 一个工具调用 C（Read 不存在的文件），流在 1040 结束，C 开工即失败。
  const a2 = msg(1031, "a2");
  const callC = { id: "toolu_c", name: "Read", input: { file_path: "/w/repo/missing.txt" } };
  const step2 = {
    finish: "tool-calls",
    usage: {
      inputTokens: 40,
      outputTokens: 6,
      totalTokens: 46,
      cacheReadTokens: 12,
      cacheWriteTokens: 0,
    },
  };
  now = 1040;
  await stepStart(a2, 1031, 9);
  await runtime.persistPart({
    id: part(10),
    sessionID: SESSION,
    messageID: a2,
    type: "reasoning",
    text: "Check the other file",
    metadata: { anthropic: { signature: "sig-2" } },
    time: { start: 1031, end: now },
  });
  const pending2 = await pendingTools(a2, [{ call: callC, partUuid: 11 }]);
  const state2 = loopState([callC]);
  // 执行器的失败结果：handler 返回业务失败 → createToolHandlerFailureError → createErrorResult
  // （modelContent 是 `<tool_use_error>…</tool_use_error>`，error.message 是原文；起止时间都是 new Date()）。
  const resultC = createErrorResult(
    callC,
    createToolHandlerFailureError(callC, {
      result: false,
      errorCode: 2,
      message: "File does not exist.",
    }),
    0,
  );
  await expect([12], () => settle(state2, a2, 1031, pending2, [resultC], step2));

  // 第三步：最后的回答（没有工具调用，同 node-store-fixtures.mjs 的一次写入）。
  const a3 = msg(1041, "a3");
  now = 1050;
  const tokens3 = toTokenUsageInfo({ inputTokens: 60, outputTokens: 9, totalTokens: 69 });
  await persistence.persistAssistantMessage.call(
    runtime,
    a3,
    u1,
    1041,
    { completed: now, finish: "stop", tokens: tokens3 },
    trace,
    model,
  );
  for (const value of [
    { id: part(13), type: "step-start" },
    {
      id: part(14),
      type: "text",
      text: "logo.png is an image; a.txt says hello.",
      time: { start: 1041, end: now },
    },
    { id: part(15), type: "step-finish", reason: "stop", cost: 0, tokens: tokens3 },
  ]) {
    await runtime.persistPart({ sessionID: SESSION, messageID: a3, ...value });
  }

  // 回合结束：TurnComplete 事件经 appendEvent → persistDurableSessionEvent 写 lastOutcome。
  now = 1051;
  const complete = runtime.createEvent(
    SessionEventType.TurnComplete,
    { response: "", tokenCount: 0, toolCallCount: 3, duration: 49, resultType: "success" },
    trace,
  );
  await runtime.appendEvent(complete, trace);
  store.close();

  const db = new DatabaseSync(dbPath, { readOnly: true });
  const rows = (sql) =>
    db
      .prepare(sql)
      .all()
      .map((row) => Object.values(row));
  const fixture = {
    generatedBy: "scripts/rust-cli/node-store-fixtures-tools.mjs",
    sessionID: SESSION,
    tables: {
      session: rows("select * from session"),
      message: rows("select * from message order by sequence"),
      part: rows("select * from part order by message_id, sequence"),
      session_entry: rows("select * from session_entry order by id"),
    },
  };
  db.close();
  const out = join(
    repoRoot,
    "packages/rust-cli/crates/node-store/fixtures/node-records-tools.json",
  );
  await writeFile(out, `${JSON.stringify(fixture, null, 2)}\n`);
  console.log(`wrote ${out}`);
} finally {
  await rm(dir, { recursive: true, force: true });
}
