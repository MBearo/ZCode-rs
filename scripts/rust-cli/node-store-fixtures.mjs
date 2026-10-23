#!/usr/bin/env node
// 生成 node-store 的 Node 记录夹具（docs/specs/rust-cli/node-store.md §6）。
//
// 用 Node 的真实代码（SqliteSessionStore、persistUserPrompt、persistAssistantMessage、
// buildExecutionStateEntry、toTokenUsageInfo、titleFromInput）在固定时钟与固定 uuid 下写一个文本会话，
// 把各表的行写进 packages/rust-cli/crates/node-store/fixtures/node-records.json。
// crates/node-store/src/records_tests.rs 用 NodeStore 提交同一组事实，要求各表逐字节相同。
// 两边的操作序列必须一致：改这里时同步改 records_tests.rs。
//
// 用法：node scripts/rust-cli/node-store-fixtures.mjs [--node-root <装好依赖的检出>]
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { ensureTypeScript, importNode, nodeRootFromArgs, repoRoot } from "./node-root.mjs";

const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
ensureTypeScript(import.meta.filename, nodeRoot, args);

// 固定时钟与 uuid：与 records_tests.rs 的 FixedSource 相同（第 n 个 uuid 是 …-<n 补足 12 位>）。
let now = 0;
Date.now = () => now;
const uuid = (n) => `00000000-0000-4000-8000-${String(n).padStart(12, "0")}`;
let queue = [];
Object.defineProperty(globalThis.crypto, "randomUUID", {
  configurable: true,
  value: () => {
    if (!queue.length) throw new Error("unexpected randomUUID call");
    return queue.shift();
  },
});

const { DatabaseSync } = await import("node:sqlite");
const { SqliteSessionStore } = await importNode(
  nodeRoot,
  "adapters/src/storage/session-store/sqlite-session-store.ts",
);
const persistence = await importNode(nodeRoot, "core/src/runtime/methods/message-persistence.ts");
const { buildExecutionStateEntry } = await importNode(
  nodeRoot,
  "core/src/runtime/execution-state.ts",
);
const { toTokenUsageInfo, emptyTokenUsageInfo } = await importNode(
  nodeRoot,
  "core/src/runtime/helpers/tool-result.ts",
);
const { titleFromInput, slugify, projectIdFromDirectory } = await importNode(
  nodeRoot,
  "core/src/runtime/helpers/project.ts",
);

const SESSION = "sess_records";
const DIRECTORY = "/w/repo";
const msg = (ms, suffix) => `msg_${ms.toString(36)}_${suffix}`;
const part = (n) => `part_${now.toString(36)}_${uuid(n)}`;

const dir = await mkdtemp(join(tmpdir(), "rust-cli-node-records-"));
try {
  const dbPath = join(dir, "db.sqlite");
  const store = await SqliteSessionStore.openStartup({ dbPath });
  let selection = { providerId: "p", modelId: "m", options: { reasoningLevel: "high" } };
  let execution = { mode: "build", planEnabled: false };
  const runtime = {
    sessionId: SESSION,
    sessionStore: store,
    config: {},
    getTools: () => [],
    getSessionModelSelection: () => selection,
    getPlanEnabled: () => execution.planEnabled,
    workingDirectory: DIRECTORY,
    workspaceRoot: DIRECTORY,
  };
  runtime.persistMessage = persistence.persistMessage.bind(runtime);
  runtime.persistPart = persistence.persistPart.bind(runtime);
  const setMode = async (permission, next) => {
    execution = next;
    runtime.config.mode = next.mode;
    await store.updateSession({ id: SESSION, permission: { mode: permission } });
    await store.saveSessionEntry(buildExecutionStateEntry(SESSION, next));
  };
  const user = async (id, input, turn, command, partUuid) => {
    queue = [uuid(partUuid)];
    await persistence.persistUserPrompt.call(
      runtime,
      id,
      input,
      undefined,
      { traceId: SESSION, turnId: turn },
      command ? { sourceCommandId: command } : undefined,
    );
    await store.updateSession({
      id: SESSION,
      title: titleFromInput(input),
      titleSource: "first_input",
      expectedTitleSources: ["default"],
    });
  };
  const assistant = async (id, parent, created, turn, { parts, finish, usage }) => {
    const tokens = usage ? toTokenUsageInfo(usage) : emptyTokenUsageInfo();
    await persistence.persistAssistantMessage.call(
      runtime,
      id,
      parent,
      created,
      { completed: now, finish, tokens },
      { traceId: SESSION, turnId: turn },
      { providerId: selection.providerId, modelId: selection.modelId },
    );
    for (const value of parts(tokens)) {
      await runtime.persistPart({ sessionID: SESSION, messageID: id, ...value });
    }
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

  // commit 1：ModelSelected、ModeChanged(acceptEdits)。
  now = 1001;
  await store.saveSessionEntry({
    id: `${SESSION}:runtime-model-selection`,
    sessionID: SESSION,
    type: "runtime/model_selection",
    touchSession: false,
    time: { created: now, updated: now },
    data: selection,
  });
  await setMode("edit", { mode: "edit", planEnabled: false });

  // commit 2（命令 cmd_1）：user 文本。turn 用第 1 个 uuid，text part 用第 2 个。
  now = 1002;
  const turn1 = `turn_${uuid(1)}`;
  const u1 = msg(1002, "u1");
  await user(u1, 'Hello  "world"\n\ttab', turn1, "cmd_1", 2);

  // commit 3：assistant（thinking、redacted thinking、两个文本块拼成一个 text part）。
  now = 1010;
  const a1 = msg(1003, "a1");
  await assistant(a1, u1, 1003, turn1, {
    finish: "stop",
    usage: {
      inputTokens: 15,
      outputTokens: 5,
      totalTokens: 20,
      cacheReadTokens: 2,
      cacheWriteTokens: 3,
    },
    parts: (tokens) => [
      { id: part(3), type: "step-start" },
      {
        id: part(4),
        type: "reasoning",
        text: "Let me think",
        metadata: { anthropic: { signature: "sig-1" } },
        time: { start: 1003, end: now },
      },
      {
        id: part(5),
        type: "reasoning",
        text: "",
        metadata: { anthropic: { redactedData: "abc" } },
        time: { start: 1003, end: now },
      },
      { id: part(6), type: "text", text: "Hi there, done.", time: { start: 1003, end: now } },
      { id: part(7), type: "step-finish", reason: "stop", cost: 0, tokens },
    ],
  });

  // commit 4：ModeChanged(plan) → permission plan、执行状态保留 edit 并打开 plan。
  now = 1500;
  await setMode("plan", { mode: "edit", planEnabled: true });

  // commit 5：第二轮。turn 用第 8 个 uuid，text part 第 9 个。
  now = 2000;
  const turn2 = `turn_${uuid(8)}`;
  const u2 = msg(2000, "u2");
  await user(u2, "Second", turn2, undefined, 9);

  // commit 6：没有用量、max_tokens（finish length）。
  now = 2005;
  const a2 = msg(2001, "a2");
  await assistant(a2, u2, 2001, turn2, {
    finish: "length",
    parts: (tokens) => [
      { id: part(10), type: "step-start" },
      { id: part(11), type: "text", text: "Done", time: { start: 2001, end: now } },
      { id: part(12), type: "step-finish", reason: "length", cost: 0, tokens },
    ],
  });

  // commit 7：TitleChanged（Node setCustomSessionTitle）。
  now = 2100;
  await store.updateSession({ id: SESSION, title: "Renamed", titleSource: "custom" });
  store.close();

  const db = new DatabaseSync(dbPath, { readOnly: true });
  const rows = (sql) =>
    db
      .prepare(sql)
      .all()
      .map((row) => Object.values(row));
  const fixture = {
    generatedBy: "scripts/rust-cli/node-store-fixtures.mjs",
    sessionID: SESSION,
    tables: {
      session: rows("select * from session"),
      message: rows("select * from message order by sequence"),
      part: rows("select * from part order by message_id, sequence"),
      session_entry: rows("select * from session_entry order by id"),
    },
  };
  db.close();
  const out = join(repoRoot, "packages/rust-cli/crates/node-store/fixtures/node-records.json");
  await writeFile(out, `${JSON.stringify(fixture, null, 2)}\n`);
  console.log(`wrote ${out}`);
} finally {
  await rm(dir, { recursive: true, force: true });
}
