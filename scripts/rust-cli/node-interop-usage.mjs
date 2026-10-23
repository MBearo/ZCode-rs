// rust-cli 与 Node CLI 的会话互操作检查的用量段（docs/specs/rust-cli/usage.md §9）。
//
// 1. rust-cli 写一个会话的三张用量表（node_interop `usage-write`）：两次请求、一次工具调用、回合结束，加子代理
//    子会话里的一次请求；
// 2. Node 用自己的仓储写同样输入的行（`recordModelUsage` / `upsertToolUsage` / `upsertTurnUsage`，取值按
//    `usage-observability.ts` 与 `turn-tool-usage.ts`，用量经 Node 的 `normalizeUsage`），两边的行除 id、会话、
//    消息与轮次外逐列相同；Node 的 `queryTaskUsage` 对两个会话的结果相同；
// 3. rust-cli 的 `activity` / `metering` 读 Node 跑过的会话（Node 建会话、写输入与用量行）：会话数与消息数各加上
//    Node 的那一份，token 与 API 时长按两边的行算；
// 4. footer 的请求统计（usage.md §4.1，修复线 F27）：rust-cli 的 `request_stats` 读两边的行，TPS 与缓存命中率与 Node
//    调试面板（`observeSessionDebug`）对同一组请求算出的值相同。
import { importNode } from "./node-root.mjs";

/** 行里与身份有关的列（会话、消息、轮次、trace、id）；其余列两边必须相同。 */
const IDENTITY = new Set([
  "id",
  "logical_request_id",
  "session_id",
  "turn_id",
  "trace_id",
  "assistant_message_id",
  "parent_user_message_id",
  "user_message_id",
  "tool_call_id",
]);

function rowsOf(store, sql, ...params) {
  return store.db.prepare(sql).all(...params);
}

function strip(row) {
  return Object.fromEntries(Object.entries(row).filter(([key]) => !IDENTITY.has(key)));
}

export async function checkUsage(ctx) {
  const {
    nodeRoot,
    store,
    dir,
    rust,
    expectEqual,
    persistence,
    createMessageId,
    createTurnId,
    projectIdFromDirectory,
    workspace,
  } = ctx;
  const { normalizeUsage } = await importNode(
    nodeRoot,
    "adapters/src/model/runner-normalization.ts",
  );
  const { slugify } = await importNode(nodeRoot, "core/src/runtime/helpers/project.ts");
  const today = new Date().toISOString().slice(0, 10);
  const before = rust("activity", dir, "all", today, "0");

  // 1. rust-cli 写。
  const written = rust("usage-write", dir);
  const { base } = written;

  // 2. Node 写同样输入的行：一个 Node 自己建的会话、Node 的输入消息。
  const nodeSession = `sess_${crypto.randomUUID()}`;
  await store.createSession({
    id: nodeSession,
    projectID: projectIdFromDirectory(workspace),
    slug: slugify(nodeSession),
    directory: workspace,
    path: workspace,
    title: "",
    titleSource: "default",
    version: "node-interop",
    traceID: nodeSession,
    permission: { mode: "edit" },
  });
  const runtime = {
    sessionId: nodeSession,
    sessionStore: store,
    config: { mode: "edit" },
    getTools: () => [],
    getSessionModelSelection: () => ({
      providerId: "anthropic",
      modelId: "claude-opus-5-5",
      options: { reasoningLevel: "max" },
    }),
    getPlanEnabled: () => false,
    workingDirectory: workspace,
    workspaceRoot: workspace,
  };
  runtime.persistMessage = persistence.persistMessage.bind(runtime);
  runtime.persistPart = persistence.persistPart.bind(runtime);
  const turnId = createTurnId();
  const trace = { traceId: nodeSession, turnId };
  const userID = createMessageId();
  await persistence.persistUserPrompt.call(
    runtime,
    userID,
    "Count the files",
    undefined,
    trace,
    {},
  );
  // AI SDK v6 的 Anthropic 用量：输入含缓存读写。
  const usage = normalizeUsage({
    inputTokens: 16,
    outputTokens: 7,
    totalTokens: 23,
    inputTokenDetails: { cacheReadTokens: 4, cacheWriteTokens: 0 },
    outputTokenDetails: {},
  });
  // usage-observability.ts `recordModelUsageFact` 的取值。
  const modelRecord = (assistantID, startedAt, firstTokenAt, completedAt, finishReason, tools) => ({
    id: `usage_model_main_turn_${assistantID}_0`,
    logicalRequestId: assistantID,
    sessionID: nodeSession,
    turnID: turnId,
    traceID: nodeSession,
    assistantMessageID: assistantID,
    parentUserMessageID: userID,
    querySource: "main_turn",
    providerId: "anthropic",
    modelId: "claude-opus-5-5",
    reasoningLevel: "max",
    agent: "zcode-agent",
    mode: "edit",
    taskType: "interactive",
    status: "completed",
    startedAt,
    firstTokenAt,
    completedAt,
    durationMs: completedAt - startedAt,
    timeToFirstTokenMs: firstTokenAt - startedAt,
    finishReason,
    toolCallCount: tools,
    inputTokens: usage.inputTokens,
    outputTokens: usage.outputTokens,
    reasoningTokens: usage.reasoningTokens,
    cacheCreationInputTokens: usage.cacheWriteTokens,
    cacheReadInputTokens: usage.cacheReadTokens,
    providerTotalTokens: usage.totalTokens,
    retryCount: tools,
    retryable: tools > 0,
    cancelledByUser: false,
    contextExceeded: false,
    rawUsage: usage,
  });
  const [first, second] = [createMessageId(), createMessageId()];
  await store.recordModelUsage(modelRecord(first, base, base + 300, base + 900, "tool-calls", 1));
  // turn-tool-usage.ts `recordToolUsageFromResult` 的取值（Read 的结果 5 个字节）。
  await store.upsertToolUsage({
    id: `usage_tool_${nodeSession}_toolu_usage_1`,
    sessionID: nodeSession,
    turnID: turnId,
    traceID: nodeSession,
    toolCallID: "toolu_usage_1",
    toolName: "Read",
    approvalStatus: "none",
    status: "completed",
    startedAt: base + 1_000,
    firstOutputAt: base + 1_400,
    completedAt: base + 1_400,
    durationMs: 400,
    timeToFirstOutputMs: 400,
    outputBytes: 5,
    retryCount: 0,
    retryable: false,
    cancelledByUser: false,
  });
  await store.recordModelUsage(
    modelRecord(second, base + 1_500, base + 1_700, base + 2_100, "stop", 0),
  );

  const rustModels = rowsOf(
    store,
    "select * from model_usage where session_id = ? order by started_at",
    written.session,
  );
  const nodeModels = rowsOf(
    store,
    "select * from model_usage where session_id = ? order by started_at",
    nodeSession,
  );
  expectEqual(
    "model_usage rows written by rust-cli and Node",
    rustModels.map(strip),
    nodeModels.map(strip),
  );
  expectEqual(
    "rust-cli model_usage identity columns",
    rustModels.map((row) => [
      row.logical_request_id,
      row.assistant_message_id,
      row.parent_user_message_id,
    ]),
    [
      [written.messages.first, written.messages.first, written.messages.user],
      [written.messages.second, written.messages.second, written.messages.user],
    ],
  );
  const rustTurn = JSON.parse(
    store.db.prepare("select data from message where id = ?").get(written.messages.user).data,
  ).anchor.turnId;
  expectEqual(
    "rust-cli usage rows carry the turn of the input",
    [...new Set(rustModels.map((row) => row.turn_id))],
    [rustTurn],
  );
  const rustTools = rowsOf(store, "select * from tool_usage where session_id = ?", written.session);
  const nodeTools = rowsOf(store, "select * from tool_usage where session_id = ?", nodeSession);
  expectEqual(
    "tool_usage rows written by rust-cli and Node",
    rustTools.map(strip),
    nodeTools.map(strip),
  );

  // usage-observability.ts `recordTurnUsageFact`：计数与 token 是这一轮的两次请求与一次工具调用。开始时间是
  // Node 回合开始，本工程取输入消息的创建时间，结束时间各自取写入时刻：这三列不比。
  const [rustTurnRow] = rowsOf(
    store,
    "select * from turn_usage where session_id = ?",
    written.session,
  );
  await store.upsertTurnUsage({
    sessionID: nodeSession,
    turnID: turnId,
    traceID: nodeSession,
    userMessageID: userID,
    status: "completed",
    startedAt: rustTurnRow.started_at,
    firstModelStartAt: base,
    firstTokenAt: base + 300,
    completedAt: rustTurnRow.completed_at,
    durationMs: rustTurnRow.completed_at - rustTurnRow.started_at,
    timeToFirstTokenMs: base + 300 - rustTurnRow.started_at,
    modelRequestCount: 2,
    modelRetryCount: 1,
    toolCallCount: 1,
    toolErrorCount: 0,
    inputTokens: 32,
    outputTokens: 14,
    reasoningTokens: undefined,
    cacheCreationInputTokens: 0,
    cacheReadInputTokens: 8,
    computedTotalTokens: 46,
    retryable: undefined,
    cancelledByUser: false,
    contextExceeded: false,
  });
  const [nodeTurnRow] = rowsOf(store, "select * from turn_usage where session_id = ?", nodeSession);
  expectEqual(
    "turn_usage rows written by rust-cli and Node",
    strip(rustTurnRow),
    strip(nodeTurnRow),
  );
  expectEqual(
    "rust-cli turn_usage starts at the input",
    [rustTurnRow.user_message_id, rustTurnRow.turn_id],
    [written.messages.user, rustTurn],
  );

  const [child] = rowsOf(store, "select * from model_usage where session_id = ?", written.child);
  expectEqual(
    "rust-cli subagent row",
    child && [
      child.query_source,
      child.task_type,
      child.assistant_message_id,
      child.logical_request_id,
    ],
    ["subagent", "subagent_child", null, written.messages.childCall],
  );

  // Node 的查询读 rust-cli 写的行：与读 Node 写的同样输入的结果相同。
  const rustTask = await store.queryTaskUsage({ sessionID: written.session });
  const nodeTask = await store.queryTaskUsage({ sessionID: nodeSession });
  expectEqual(
    "Node queryTaskUsage over rust-cli rows",
    { ...rustTask, sessionID: undefined },
    { ...nodeTask, sessionID: undefined },
  );
  const app = await store.queryAppUsage({ since: base - 1, until: Date.now(), tzOffsetMs: 0 });
  expectEqual(
    "Node queryAppUsage reads both sessions",
    [app.totals.modelRequestCount, app.toolTotals.toolCallCount, app.turnTotals.totalTurns],
    [5, 2, 2],
  );

  // 3. rust-cli 读 Node 跑过的会话。
  const after = rust("activity", dir, "all", today, "0");
  const messages = (stats) => stats.days.reduce((sum, day) => sum + day.messages, 0);
  // rust-cli 的会话：输入、两条回复（工具结果没有消息行）；Node 的会话：一条输入；子会话不算。
  expectEqual(
    "rust-cli activity counts the Node session",
    [after.sessions - before.sessions, messages(after) - messages(before)],
    [2, 4],
  );
  // 五次请求（rust-cli 三次、Node 两次），每次不含缓存的输入 16 − 4 = 12。
  expectEqual("rust-cli activity tokens from both writers", after.models, [
    { model: "claude-opus-5-5", input: 60, output: 35, cache_read: 20, cache_write: 0 },
  ]);
  expectEqual("rust-cli metering of the Node session", rust("metering", dir, nodeSession), {
    api_duration_ms: 900 + 600,
  });
  expectEqual(
    "rust-cli metering of its own session and subagent",
    rust("metering", dir, written.session),
    {
      api_duration_ms: 900 + 600 + 500,
    },
  );

  // 4. footer 的请求统计：Node 调试面板对这两次主回合请求的结果（每次完成的请求一个 `model_request_completed`，
  // 时长与首字取 Node 写的行，用量是同一份归一化用量）。
  const { observeSessionDebug, readSessionDebug } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol/session-debug.ts",
  );
  const { SessionEventType } = await importNode(nodeRoot, "contracts/src/index.ts");
  const debugRecord = { app: { sessionId: nodeSession } };
  for (const [index, row] of nodeModels.entries())
    observeSessionDebug(debugRecord, {
      id: `usage_debug_${index}`,
      sessionId: nodeSession,
      traceId: nodeSession,
      turnId,
      sequenceNumber: index + 1,
      timestamp: new Date(row.completed_at),
      type: SessionEventType.ModelNetworkStatus,
      payload: {
        type: "model_request_completed",
        requestId: row.logical_request_id,
        querySource: row.query_source,
        providerId: row.provider_id,
        modelId: row.model_id,
        transport: "sse",
        attempt: 1,
        maxAttempts: 3,
        timestamp: new Date(row.completed_at).toISOString(),
        durationMs: row.duration_ms,
        timeToFirstContentMs: row.time_to_first_token_ms,
        usage,
        requestHeaders: {},
        responseHeaders: {},
      },
    });
  const debug = readSessionDebug(debugRecord);
  const nodeFooter = { tps: debug.rounds.at(-1).tokensPerSecond, hitRate: debug.cache.hitRate };
  // 第二次请求 600 ms、首字 200 ms、输出 7 → 7 × 1000 / 400；两次的缓存读 4 + 4、输入 16 + 16（两边都不能是空值）。
  expectEqual("Node debug panel over the two requests", nodeFooter, { tps: 17.5, hitRate: 0.25 });
  for (const [label, session] of [
    ["the Node session", nodeSession],
    ["its own session", written.session],
  ]) {
    const { tps, hitRate } = rust("request-stats", dir, session);
    expectEqual(
      `rust-cli request stats of ${label} match the Node debug panel`,
      { tps, hitRate },
      nodeFooter,
    );
  }

  // 5. 旁路请求的行（工作线 F71，usage.md §2.1）：rust-cli 在另一个会话里写会话标题与 WebFetch 摘要的两次请求，
  // `query_source` 与行 id 用 Node 的名字（`session_title`、`web_fetch_processing`）；Node 对同样输入写的行逐列相同。
  // 放在最后，不影响上面的会话数与合计。
  const side = rust("usage-side", dir);
  const nodeSide = `sess_${crypto.randomUUID()}`;
  await store.createSession({
    id: nodeSide,
    projectID: projectIdFromDirectory(workspace),
    slug: slugify(nodeSide),
    directory: workspace,
    path: workspace,
    title: "",
    titleSource: "default",
    version: "node-interop",
    traceID: nodeSide,
    permission: { mode: "edit" },
  });
  for (const [querySource, at] of [
    ["session_title", side.base],
    ["web_fetch_processing", side.base + 1_000],
  ]) {
    const id = createMessageId();
    await store.recordModelUsage({
      ...modelRecord(id, at, at + 100, at + 400, "stop", 0),
      id: `usage_model_${querySource}_${id}_0`,
      sessionID: nodeSide,
      traceID: nodeSide,
      assistantMessageID: undefined,
      querySource,
    });
  }
  const sideRows = (session) =>
    rowsOf(store, "select * from model_usage where session_id = ? order by started_at", session);
  const rustSide = sideRows(side.session);
  expectEqual(
    "rust-cli side request rows use the Node names",
    rustSide.map((row) => [row.id, row.query_source, row.assistant_message_id]),
    [
      [`usage_model_session_title_${side.messages.title}_0`, "session_title", null],
      [`usage_model_web_fetch_processing_${side.messages.fetch}_0`, "web_fetch_processing", null],
    ],
  );
  expectEqual(
    "side request rows written by rust-cli and Node",
    rustSide.map(strip),
    sideRows(nodeSide).map(strip),
  );
}
