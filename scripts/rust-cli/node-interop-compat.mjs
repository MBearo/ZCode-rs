// rust-cli 与 Node 共用数据的兼容检查（修复线 F2，docs/specs/rust-cli/node-store.md §6 第 10 步）。
//
// 由 node-interop-check.mjs 调用，用同一个库与 Node 的 store：
// 1. App 协议：库里每个会话的每条消息过 App 的 strict schema（`mapMessageWithParts` + `zcodeMessageWithPartsSchema`），
//    v4 的事件合成不报错（含流式中被中断的 assistant）；
// 2. 回合结果与模型：中断的会话 `lastOutcome = interrupted`；assistant 的 `modelId` 是请求的目录 ID；中断的
//    step-finish 与 assistant 带 Node 的 `unknown`；
// 3. bash 模式与 skill 命令：界面原文在 text、模型文本在 `metadata.modelInput`，输出是合成消息，只有两个真实输入轮次；
//    Node 重建的上下文与 rust-cli 的冷读取相同（含 `plan_mode_exit` 提醒）；
// 4. 一次性子代理：Node 的子代理查询找到子会话；
// 5. checkpoint 混写：Node 先写一个序号较大的 checkpoint，rust-cli 再写两个，Node 恢复的顺序与写入顺序相同、序号不重复；
// 6. Node 群协作会话（`permission.scope = session`）：rust-cli 的“总是允许”与改模式只写会话行，项目规则与持久模式不变；
// 7. todo：Node 写的 todo，rust-cli 读成任务清单，追加后 Node 的 todo 还在；
// 8. provider_config.json：不存在且旧 config.json 有 provider 时，rust-cli 不抢先建文件，Node 照常导入；
// 9. 存储位置：`zcode -p` 按 Node 的顺序解析 `ZCODE_SESSION_DB_PATH` 与 `storage.dir`，Node 按自己的解析找到这个库。
import { execFileSync, spawn } from "node:child_process";
import { mkdir, rm, writeFile } from "node:fs/promises";
import { existsSync, realpathSync } from "node:fs";
import { createServer } from "node:http";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { binaryPath } from "./binary.mjs";
import { checkAppInterop } from "./node-interop-app.mjs";
import { appSchemaRejections, nodeShape, rustShape } from "./node-interop-history.mjs";
import { importNode, withHome } from "./node-root.mjs";
import { GLM_5_3, GLM_5_3_ACCOUNT } from "./fixture/model.mjs";

export async function checkCompat(ctx) {
  const { nodeRoot, store, dir, rust, expectEqual, nodeHistory, rustText, artifactStore, written } =
    ctx;
  const compat = rust("compat-write", dir);

  // 1. App 协议的 strict schema（node-interop-history.mjs，第十一段也用）。
  const sessions = await store.listSessions({ directory: written.workspace });
  const ids = new Set([...sessions.map((s) => s.id), compat.child]);
  const rejected = await appSchemaRejections(nodeRoot, store, ids);
  expectEqual("every message passes the App protocol schema", rejected, []);
  expectEqual("the App schema saw the compat sessions", ids.has(compat.interrupted), true);

  // 2. 回合结果、模型 ID 与中断的停止原因。
  const interrupted = await store.getSession(compat.interrupted);
  expectEqual("lastOutcome of the interrupted turn", interrupted.lastOutcome, "interrupted");
  const assistants = (await store.messages({ sessionID: compat.interrupted })).filter(
    (m) => m.info.role === "assistant",
  );
  expectEqual(
    "assistant modelId is the requested catalog id",
    assistants.map((m) => m.info.modelId),
    ["m", "m"],
  );
  const stepFinish = assistants[1].parts.find((part) => part.type === "step-finish");
  expectEqual(
    "the interrupted step records Node's unknown finish",
    [assistants[1].info.finish, stepFinish?.reason],
    ["unknown", "unknown"],
  );
  const interruptedLoaded = rust("read", dir, compat.interrupted);
  expectEqual(
    "rust-cli reads the interrupted step back as unknown",
    interruptedLoaded.messages.filter((m) => m.type === "assistant").map((m) => m.stop_reason),
    ["end_turn", "unknown"],
  );
  expectEqual(
    "history of the interrupted session",
    await nodeHistory(store, artifactStore, interrupted, compat.interrupted),
    rustText(interruptedLoaded.messages),
  );

  // 2b. 输入还没提交时被中断（修复线 F37，session-lifecycle.md §3.1）：输入、当步的 attachment、上下文宣布与中断标记，
  // 没有 assistant。Node 读出被中断的回合、只有这一条真实输入、重建的上下文与 rust-cli 的冷读取相同；Node 续写一轮
  // 之后 rust-cli 读回，中断标记仍在 Node 的输入之前。
  await checkEarlyInterrupt(ctx, compat.early);
  // 2c. esc 撤回刚发的输入（修复线 F38，session-lifecycle.md §5.5）：同 2b 的输入之后只有一个 `session.revert`。Node 的
  // 活动分支只剩第一轮（撤回的输入、宣布与标记都不在），续写一轮之后 rust-cli 读回，人类输入是第一轮与 Node 的输入。
  await checkRestored(ctx, compat.restored);

  // 3. bash 模式与 skill 命令的显示、轮次与模型文本。
  const displayedMessages = await store.messages({ sessionID: compat.displayed });
  const realUsers = displayedMessages.filter((m) => m.info.semantics?.origin === "real_user");
  expectEqual(
    "real user inputs show the typed text",
    realUsers.map((m) => m.parts.filter((p) => p.type === "text").map((p) => p.text)),
    [["! ls"], ["/review main"]],
  );
  expectEqual(
    "the model text is in metadata.modelInput",
    realUsers.map((m) => m.parts[0]?.metadata?.modelInput),
    [
      "<bash-input>ls</bash-input>",
      "<command-message>review</command-message>\n<command-name>/review</command-name>\n<command-args>main</command-args>",
    ],
  );
  const turns = new Set(
    displayedMessages
      .filter((m) => m.info.role === "user")
      .map((m) => m.info.anchor?.turnId)
      .filter(Boolean),
  );
  expectEqual("bash output and the skill body open no new turn", turns.size, 2);
  const output = displayedMessages.find((m) =>
    m.parts.some((p) => p.type === "text" && p.text.startsWith("<bash-stdout>")),
  );
  expectEqual("the bash output is a synthetic message", output?.info.synthetic, true);
  const displayed = await store.getSession(compat.displayed);
  expectEqual("the first-input title shows the typed text", displayed.title, "! ls");
  const displayedLoaded = rust("read", dir, compat.displayed);
  // 工作线 F46，登记的差异（需要用户确认，README §11 F46）：staging 的 Node 冷读取不读 `metadata.modelInput`
  // （`textPartToProviderText` 的非合成分支只取 `text`；来源分支 b3b0aa8195 里 Node 的那一半没有进 staging），这两条
  // 真实输入 Node 给模型的是界面原文，本工程给的是模型文本。只换这两条，其余仍要与本工程的冷读取相同；Node 读
  // modelInput 之后这条断言会失败，那时改回直接比 `rustText(displayedLoaded.messages)`。
  const shownFor = new Map(
    realUsers
      .map((m) => m.parts[0])
      .filter((part) => part?.metadata?.modelInput)
      .map((part) => [part.metadata.modelInput, part.text]),
  );
  expectEqual(
    "history of the bash and skill session (model text, plan_mode_exit)",
    await nodeHistory(store, artifactStore, displayed, compat.displayed),
    rustText(displayedLoaded.messages).map((item) =>
      item.role === "user" && shownFor.has(item.text) ? { ...item, text: shownFor.get(item.text) } : item,
    ),
  );
  expectEqual(
    "rust-cli reads the bash output back as the human message",
    displayedLoaded.messages
      .filter((m) => m.type === "user" && m.origin.kind === "human")
      .map((m) => m.content[0].text.slice(0, 13)),
    ["<bash-input>l", "<bash-stdout>", "<command-mess"],
  );
  // `plan_mode_exit` 与 Node 提醒表重名：存成 `rust-cli/plan_mode_exit`，Node 不把它当成自己的运行时提醒。
  const exitPart = displayedMessages
    .flatMap((m) => m.parts)
    .find((part) => part.type === "text" && part.text.includes("Exited Plan Mode"));
  expectEqual("plan_mode_exit is stored under a rust-cli source", exitPart?.metadata?.source, "rust-cli/plan_mode_exit");
  expectEqual(
    "rust-cli reads plan_mode_exit back under its own source",
    displayedLoaded.messages.filter((m) => m.type === "attachment").map((m) => m.source),
    ["plan_mode_exit"],
  );

  // 历史的消息结构（原来只比拼接后的文本）：没有回退与压缩的会话，每条 Node 行的角色、`semantics.origin`、
  // `synthetic` 与 text part 数，与 rust-cli 冷读取的消息逐条对应。
  for (const sessionID of [written.sessions[0], written.sessions[2], compat.interrupted, compat.displayed]) {
    expectEqual(
      `${sessionID} message structure`,
      nodeShape(await store.messages({ sessionID })),
      rustShape(rust("read", dir, sessionID).messages),
    );
  }

  // 4. 一次性子代理在 Node 的子代理查询里可见。
  const { collectSubagentChildSessionIds } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol/subagent-session-query.ts",
  );
  const parent = await store.getSession(compat.subagent);
  expectEqual(
    "Node finds the one-shot subagent's child session",
    collectSubagentChildSessionIds(parent, await store.messages({ sessionID: compat.subagent })),
    [compat.child],
  );

  // 5. checkpoint 混写的顺序。
  await checkCheckpointOrder({ ...ctx, session: compat.displayed });

  // 6. Node 群协作会话由 rust-cli 续写。
  await checkSessionScope({ ...ctx, session: written.sessions[1] });

  // 7. todo 的导入与保留。
  await store.updateTodos({
    sessionID: compat.displayed,
    todos: [{ content: "Node task", status: "in_progress", priority: "high" }],
  });
  expectEqual(
    "rust-cli imports Node's todo",
    rust("task-load", dir, compat.displayed).tasks.map((t) => [t.id, t.subject, t.status]),
    [["1", "Node task", "in_progress"]],
  );
  rust("task-add", dir, compat.displayed, "Rust task");
  expectEqual(
    "Node keeps its todo after rust-cli adds a task",
    await store.readTodos({ sessionID: compat.displayed }),
    [
      { content: "Node task", status: "in_progress", priority: "high" },
      { content: "Rust task", status: "pending", priority: "medium" },
    ],
  );

  // App 协议的会话互读（node-interop-app.mjs，app-protocol.md §10.4 第 3 条；只在本仓库）。
  const app = checkAppInterop();
  if (!app.skipped) expectEqual(app.label, app.output, app.expected);
}

/** Node 先写一个序号较大的 checkpoint（它的事件计数器），rust-cli 再写两个；Node 恢复的顺序与写入顺序相同。 */
async function checkCheckpointOrder({ nodeRoot, store, dir, rust, expectEqual, session }) {
  const { persistWorkspaceCheckpointEntry, restoreWorkspaceCheckpointEntries } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/workspace-checkpoint-persistence.ts",
  );
  const { createSessionEvent, SessionEventType } = await importNode(
    nodeRoot,
    "contracts/src/events/session.events.ts",
  );
  const { createInMemorySessionEventStore } = await importNode(
    nodeRoot,
    "contracts/src/events/in-memory-session-event-store.ts",
  );
  const trace = { traceId: session };
  const target = (await store.messages({ sessionID: session })).find(
    (m) => m.info.role === "user",
  ).info.id;
  const event = createSessionEvent(
    SessionEventType.CheckpointCreated,
    session,
    {
      checkpointId: "checkpoint_node",
      messageId: target,
      targetMessageId: target,
      scope: "workspace",
      snapshotRef: "zcode-artifact://node/snapshot",
      diffRef: "zcode-artifact://node/snapshot",
      fileCount: 1,
    },
    { traceId: session },
  );
  event.sequenceNumber = 57;
  await persistWorkspaceCheckpointEntry({ sessionStore: store }, event, trace);
  rust("compat-checkpoints", dir, session);
  const eventStore = createInMemorySessionEventStore();
  await restoreWorkspaceCheckpointEntries({ sessionId: session, sessionStore: store, eventStore }, trace);
  const restored = await eventStore.getEvents(session);
  expectEqual(
    "Node restores the checkpoints in write order",
    restored.map((e) => e.payload.checkpointId),
    ["checkpoint_node", "checkpoint_toolu_b1", "checkpoint_toolu_b2"],
  );
  const sequences = restored.map((e) => e.sequenceNumber);
  expectEqual(
    "checkpoint sequence numbers grow without repeats",
    sequences.every((value, index) => index === 0 || value > sequences[index - 1]),
    true,
  );
}

/** Node 把会话收紧成群协作授权并“总是允许” Read；rust-cli 续写后，规则与模式只在会话行里。 */
async function checkSessionScope({ nodeRoot, store, dir, rust, expectEqual, written, session }) {
  const { ensureGroupTaskPermissionScope } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/input-intent-persistence.ts",
  );
  const { persistProjectPermissionUpdates, loadProjectPermissionRuleset } = await importNode(
    nodeRoot,
    "core/src/tool/executor/permission-rules-persistence.ts",
  );
  const { projectIdFromDirectory } = await importNode(
    nodeRoot,
    "core/src/runtime/helpers/project.ts",
  );
  const projectID = projectIdFromDirectory(written.workspace);
  const projectRules = await store.getProjectPermission(projectID);
  const projectMode = store.getProjectPermissionMode(projectID);
  await ensureGroupTaskPermissionScope(store, session, { botGroupSource: { kind: "group" } });
  const deps = { sessionStore: store, sessionId: session };
  const trace = { traceId: session };
  await persistProjectPermissionUpdates(
    deps,
    [{ type: "addRules", behavior: "allow", rules: [{ toolName: "Read" }] }],
    trace,
  );
  const read = rust("compat-scope", dir, session);
  const scoped = (await store.getSession(session)).permission;
  expectEqual("the session ruleset keeps Node's scope and gains the rule", scoped, {
    version: 1,
    scope: "session",
    mode: "edit",
    allow: [{ toolName: "Read" }, { toolName: "Bash", ruleContent: "npm test:*" }],
  });
  expectEqual(
    "Node's loadProjectPermissionRuleset reads the same session rules",
    await loadProjectPermissionRuleset(deps),
    scoped,
  );
  expectEqual(
    "the project ruleset is not touched",
    await store.getProjectPermission(projectID),
    projectRules,
  );
  expectEqual(
    "the project mode is not touched",
    store.getProjectPermissionMode(projectID),
    projectMode,
  );
  expectEqual("rust-cli reads the session rules", read.session, {
    allow: [{ tool: "Read" }, { tool: "Bash", content: "npm test:*" }],
  });
  expectEqual("rust-cli reads the mode it set", read.mode, "acceptEdits");
}

/**
 * provider_config.json 不存在时（tui-manage.md §2.2，platform.md §2）：旧 `~/.zcode/cli/config.json` 有 Node 要导入的
 * provider 时，rust-cli 不建文件，Node 照常导入；没有要导入的内容时，rust-cli 写的默认选择 Node 读得出。
 */
/** 2b（修复线 F37）：提交前被中断的会话，Node 读、续写，rust-cli 读回。 */
async function checkEarlyInterrupt(ctx, sessionID) {
  const { store, dir, rust, expectEqual, nodeHistory, rustText, artifactStore } = ctx;
  const {
    nodeRuntime,
    persistence,
    createMessageId,
    createPartId,
    createTurnId,
    toTokenUsageInfo,
  } = ctx.node;
  const session = await store.getSession(sessionID);
  expectEqual(
    "lastOutcome of the turn interrupted before its request",
    session.lastOutcome,
    "interrupted",
  );
  const messages = await store.messages({ sessionID });
  expectEqual(
    "the input interrupted before its request is the only real user input",
    messages
      .filter((m) => m.info.semantics?.origin === "real_user")
      .map((m) => m.parts.filter((p) => p.type === "text").map((p) => p.text)),
    [["Fix the build"]],
  );
  expectEqual(
    "no assistant for a turn interrupted before its request",
    messages.filter((m) => m.info.role === "assistant").length,
    0,
  );
  expectEqual(
    `${sessionID} message structure`,
    nodeShape(messages),
    rustShape(rust("read", dir, sessionID).messages),
  );
  expectEqual(
    "history of the session interrupted before its request",
    await nodeHistory(store, artifactStore, session, sessionID),
    rustText(rust("read", dir, sessionID).messages),
  );
  // Node 续写一轮 user / assistant（Node 自己的写入代码，同 node-interop-check.mjs 的第一个会话）。
  const { runtime, selection } = await nodeRuntime(sessionID);
  const trace = { traceId: sessionID, turnId: createTurnId() };
  const userID = createMessageId();
  await persistence.persistUserPrompt.call(runtime, userID, "Try again", undefined, trace, {
    sourceCommandId: "node_cmd_early",
  });
  const assistantID = createMessageId();
  const created = Date.now();
  const tokens = toTokenUsageInfo({ inputTokens: 20, outputTokens: 2, totalTokens: 22 });
  await persistence.persistAssistantMessage.call(
    runtime,
    assistantID,
    userID,
    created,
    { completed: Date.now(), finish: "stop", tokens },
    trace,
    { providerId: selection.providerId, modelId: selection.modelId },
  );
  for (const part of [
    { type: "step-start" },
    { type: "text", text: "Fixed.", time: { start: created, end: Date.now() } },
    { type: "step-finish", reason: "stop", cost: 0, tokens },
  ]) {
    await runtime.persistPart({ id: createPartId(), sessionID, messageID: assistantID, ...part });
  }
  const continued = await store.getSession(sessionID);
  const loaded = rust("read", dir, sessionID);
  expectEqual(
    "history after Node continued the session interrupted before its request",
    await nodeHistory(store, artifactStore, continued, sessionID),
    rustText(loaded.messages),
  );
  expectEqual(
    "last two messages after Node continued the session interrupted before its request",
    loaded.messages.slice(-2).map((m) => m.id),
    [userID, assistantID],
  );
  const humans = loaded.messages
    .filter((m) => m.type === "user" && m.origin.kind === "human")
    .map((m) => m.content[0].text);
  expectEqual("rust-cli reads the input, the marker and Node's input in order", humans, [
    "Fix the build",
    "[Request interrupted by user]",
    "Try again",
  ]);
}

/** 2c（修复线 F38）：esc 撤回的会话，Node 读（活动分支不含撤回的输入）、续写，rust-cli 读回。 */
async function checkRestored(ctx, sessionID) {
  const { store, dir, rust, expectEqual, nodeHistory, rustText, artifactStore } = ctx;
  const {
    nodeRuntime,
    persistence,
    createMessageId,
    createPartId,
    createTurnId,
    toTokenUsageInfo,
  } = ctx.node;
  const session = await store.getSession(sessionID);
  expectEqual(
    "the restored session keeps Node's revert cursor",
    [session.revert?.kind, session.revert?.branchGeneration],
    ["conversation_rewind", 1],
  );
  expectEqual(
    `${sessionID} history without the restored input`,
    await nodeHistory(store, artifactStore, session, sessionID),
    rustText(rust("read", dir, sessionID).messages),
  );
  const read = rust("read", dir, sessionID).messages;
  expectEqual(
    "rust-cli reads only the first turn after the restore",
    read.map((m) => (m.type === "user" ? m.content[0].text : m.type)),
    ["Say hi", "assistant"],
  );
  const { runtime, selection } = await nodeRuntime(sessionID);
  const trace = { traceId: sessionID, turnId: createTurnId() };
  const userID = createMessageId();
  await persistence.persistUserPrompt.call(runtime, userID, "Fix the build", undefined, trace, {
    sourceCommandId: "node_cmd_restored",
  });
  const assistantID = createMessageId();
  const created = Date.now();
  const tokens = toTokenUsageInfo({ inputTokens: 20, outputTokens: 2, totalTokens: 22 });
  await persistence.persistAssistantMessage.call(
    runtime,
    assistantID,
    userID,
    created,
    { completed: Date.now(), finish: "stop", tokens },
    trace,
    { providerId: selection.providerId, modelId: selection.modelId },
  );
  for (const part of [
    { type: "step-start" },
    { type: "text", text: "Fixed.", time: { start: created, end: Date.now() } },
    { type: "step-finish", reason: "stop", cost: 0, tokens },
  ]) {
    await runtime.persistPart({ id: createPartId(), sessionID, messageID: assistantID, ...part });
  }
  const continued = await store.getSession(sessionID);
  const loaded = rust("read", dir, sessionID);
  expectEqual(
    "history after Node continued the restored session",
    await nodeHistory(store, artifactStore, continued, sessionID),
    rustText(loaded.messages),
  );
  expectEqual(
    "rust-cli reads the first turn and Node's input after the restore",
    loaded.messages
      .filter((m) => m.type === "user" && m.origin.kind === "human")
      .map((m) => m.content[0].text),
    ["Say hi", "Fix the build"],
  );
  expectEqual(
    "last two messages after Node continued the restored session",
    loaded.messages.slice(-2).map((m) => m.id),
    [userID, assistantID],
  );
}

export async function checkProviderImport({ nodeRoot, example, home, expectEqual }) {
  // 被拒时 ui_prefs 以非 0 退出、原因写 stderr：这里收起来，不混进检查的输出。
  const uiPrefs = (...command) =>
    JSON.parse(
      execFileSync(example("ui_prefs"), command, {
        encoding: "utf8",
        stdio: ["ignore", "pipe", "pipe"],
      }),
    );
  const { NodePersonalProviderConfigRepository } = await import(
    pathToFileURL(join(nodeRoot, "packages/provider-node/src/index.ts")).href
  );
  const { readLegacyCliPersonalProviderConfig } = await importNode(
    nodeRoot,
    "bootstrap/src/app/legacy-cli-personal-provider-config-importer.ts",
  );
  const legacy = join(home, ".zcode", "cli", "config.json");
  const file = join(home, ".zcode", "v2", "provider_config.json");
  await mkdir(join(home, ".zcode", "cli"), { recursive: true });
  await writeFile(
    legacy,
    `${JSON.stringify(
      {
        provider: {
          "my-proxy": {
            kind: "anthropic",
            name: "My Proxy",
            options: { apiKey: "sk-test", baseURL: "https://llm.example.com" },
            models: { m1: {} },
          },
        },
        model: "my-proxy/m1",
      },
      null,
      2,
    )}\n`,
  );
  let refused = false;
  try {
    uiPrefs("model-write", home, "zai", GLM_5_3_ACCOUNT, "high");
  } catch {
    refused = true;
  }
  expectEqual("rust-cli leaves the legacy import to Node", [refused, existsSync(file)], [true, false]);
  const repository = new NodePersonalProviderConfigRepository({
    filePath: file,
    pollingIntervalMs: false,
    importLegacy: () => readLegacyCliPersonalProviderConfig({ filePath: legacy }),
  });
  const imported = await repository.read();
  repository.dispose();
  expectEqual(
    "Node imports the legacy provider and default",
    [imported.providers.toJSON().map((rule) => rule.providerId), imported.defaultModelSelection],
    [["my-proxy"], { providerId: "my-proxy", modelId: "m1" }],
  );
  // 旧文件没有要导入的内容：rust-cli 写，Node 读出同一个默认选择。
  await rm(file, { force: true });
  await writeFile(legacy, `${JSON.stringify({ ui: { locale: "en-US" } }, null, 2)}\n`);
  uiPrefs("model-write", home, "zai", GLM_5_3_ACCOUNT, "high");
  const fresh = new NodePersonalProviderConfigRepository({
    filePath: file,
    pollingIntervalMs: false,
    importLegacy: () => readLegacyCliPersonalProviderConfig({ filePath: legacy }),
  });
  expectEqual("Node reads the default rust-cli wrote", (await fresh.read()).defaultModelSelection, {
    providerId: "zai",
    modelId: GLM_5_3_ACCOUNT,
    options: { reasoningLevel: "high" },
  });
  fresh.dispose();
}

const REPLY = [
  'event: message_start\ndata: {"type":"message_start","message":{"id":"msg_1","type":"message","role":"assistant","model":"m","content":[],"stop_reason":null,"usage":{"input_tokens":1,"output_tokens":1}}}\n\n',
  'event: content_block_start\ndata: {"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}\n\n',
  'event: content_block_delta\ndata: {"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"done"}}\n\n',
  'event: content_block_stop\ndata: {"type":"content_block_stop","index":0}\n\n',
  'event: message_delta\ndata: {"type":"message_delta","delta":{"stop_reason":"end_turn"},"usage":{"output_tokens":1}}\n\n',
  'event: message_stop\ndata: {"type":"message_stop"}\n\n',
].join("");

/**
 * 存储位置（README §5、node-store.md §6 第 10 步）：真实的 `zcode -p` 在临时 home 里跑一轮，库与产物目录按 Node 的
 * 顺序解析（`ZCODE_SESSION_DB_PATH` 覆盖 `storage.sessionDbPath`，`storage.dir` 来自用户级配置）；Node 用自己的
 * `createConfig` + `getSessionDbPath` 找到同一个库并列出这次的会话。
 */
export async function checkStoragePaths({ nodeRoot, repoRoot, base, expectEqual }) {
  const manifest = join(repoRoot, "packages/rust-cli/Cargo.toml");
  // 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml（工作线 F46）。
  execFileSync("cargo", ["build", "-q", "--manifest-path", manifest, "-p", "rust-cli"], {
    cwd: dirname(manifest),
    stdio: "inherit",
  });
  const zcode = binaryPath(repoRoot, "debug");
  const { createConfig } = await importNode(nodeRoot, "adapters/src/config/config-factory.ts");
  const { getSessionDbPath } = await importNode(nodeRoot, "bootstrap/src/app/session-store.ts");
  const { SqliteSessionStore } = await importNode(
    nodeRoot,
    "adapters/src/storage/session-store/sqlite-session-store.ts",
  );
  const home = join(base, "home");
  await mkdir(join(base, "work", ".git"), { recursive: true });
  // 会话行的目录是进程的 cwd（macOS 的临时目录经符号链接解析成 /private/…）。
  const work = realpathSync(join(base, "work"));
  await mkdir(join(home, ".zcode", "cli"), { recursive: true });
  await writeFile(
    join(home, ".zcode", "cli", "config.json"),
    `${JSON.stringify({ storage: { dir: join(base, "store"), sessionDbPath: join(base, "config.sqlite") } }, null, 2)}\n`,
  );
  const env = {
    HOME: home,
    PATH: process.env.PATH,
    ZCODE_DATA_BASE_DIR: home,
    ZCODE_SESSION_DB_PATH: join(base, "env.sqlite"),
  };
  const server = createServer((request, response) => {
    request.resume();
    request.on("end", () => {
      response.writeHead(200, { "content-type": "text/event-stream" });
      response.end(REPLY);
    });
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  try {
    const { port } = server.address();
    const code = await new Promise((resolve, reject) => {
      const child = spawn(zcode, ["-p", "storage probe"], {
        cwd: work,
        env: {
          ...env,
          // rust-cli 只读 ZCODE_* 的名字（修复线 F42，config.md §2.5）。
          ZCODE_MODEL_BASE_URL: `http://127.0.0.1:${port}`,
          ZCODE_API_KEY: "test-key",
          ZCODE_MODEL: GLM_5_3,
        },
        stdio: ["ignore", "ignore", "inherit"],
      });
      child.on("error", reject);
      child.on("exit", resolve);
    });
    expectEqual("zcode -p with the storage settings exits 0", code, 0);
  } finally {
    server.close();
  }
  // 修复线 F50：`createConfig` 的用户配置按本进程的 `os.homedir()` 找（不看 env），以前读的是本机真实的
  // `~/.zcode/cli/config.json`，不是上面写进临时 home 的那份（“env 盖过 storage.sessionDbPath”其实没有比到）。
  const config = await withHome(home, () => createConfig({ env, workingDirectory: work }));
  const dbPath = getSessionDbPath(config);
  expectEqual("Node resolves ZCODE_SESSION_DB_PATH over storage.sessionDbPath", dbPath, env.ZCODE_SESSION_DB_PATH);
  expectEqual("rust-cli wrote the session to the Node db path", existsSync(dbPath), true);
  expectEqual(
    "rust-cli did not use the default db",
    existsSync(join(home, ".zcode", "cli", "db", "db.sqlite")),
    false,
  );
  if (!existsSync(dbPath)) return;
  const store = await SqliteSessionStore.openStartup({ dbPath });
  try {
    const listed = await store.listSessions({ directory: work, roots: true });
    expectEqual("Node lists the zcode -p session", listed.length, 1);
  } finally {
    store.close();
  }
}
