#!/usr/bin/env node
// rust-cli 与 Node CLI 的会话互操作检查（docs/specs/rust-cli/node-store.md §6，README §9）。
//
// 1. rust-cli 的 NodeStore 写会话（crates/node-store/examples/node_interop.rs `write`）：文本回合、
//    回退切点，以及带 attachment（含提及的图片与 notebook 的内容块）、工具调用与结果（含图片）、人类输入里粘贴的图片、
//    中断、meta 消息的会话；
// 2. Node 的 SqliteSessionStore 打开同一个库（迁移账本必须被接受），读会话行、列表、模型选择、
//    执行状态，用 history hydrator 重建模型上下文，与 rust-cli 的冷读取（`read`）比对文本、推理、
//    工具调用与工具结果；
// 3. Node 用自己的写入代码续写：文本会话续一轮并改执行状态，工具会话续一轮带工具调用的回合；
//    本机项目规则集（“总是允许”与 `/permissions` 的加删）与项目的持久模式两边互读（shell-permissions.md §5、
//    tui-manage.md §1.1、config.md §2.2），
//    Node 读出生成的标题（titles.md）；
//    rust-cli 再冷读取，结果与 Node 一致。
// 7. Node 的 `/fork`（session-lifecycle.md §6.5，`node-interop-fork.mjs`）：rust-cli 分出的会话 Node 能读、能续写，
//    Node 分出的会话 rust-cli 能读。
// 8. 新会话的默认模型（workspace-trust.md §3，`node-interop-model.mjs`）：五种缺省下两边解析出同一个初始模型，
//    一边写、另一边解析，同一份输入写出的文本逐字节相同。
// 9. 用量表（usage.md §9，`node-interop-usage.mjs`）：rust-cli 写的三张表与 Node 同样输入写的行逐列相同、Node 的
//    查询能读；rust-cli 的 `activity` / `metering` 读 Node 跑过的会话；旁路请求（标题、WebFetch 摘要）的行用 Node 的
//    名字（工作线 F71）。
// 10. 与 Node 共用数据的兼容（修复线 F2，`node-interop-compat.mjs`）：App 协议 schema、中断的步骤、模型 ID、
//    lastOutcome、bash / skill 的显示、子代理关联、checkpoint 混写、群协作会话的规则、todo、旧 provider 导入、
//    存储位置（真实的 `zcode -p`）；提交前被中断的输入（修复线 F37）与 esc 撤回的会话（修复线 F38，只有
//    `session.revert`）Node 能读、能续写。
// 11. Node 的真实运行时（修复线 F12，`node-interop-runtime.mjs`）：bootstrap 的 `createZCodeApp` 跑真实回合、回退、
//    文件回退与压缩，与真实的 `zcode -p` 和 `node_kernel`（`/compact`、`/rewind`、`/branch`）轮流写同一个会话，
//    比模型请求里的历史、冷读取、消息结构、App 协议 schema 与两边的改动摘要。
// 12. 双进程并发写与迁移竞争（修复线 F12，`node-interop-race.mjs`）：不同会话、同一会话、锁与等待、迁移竞争。
// 13. 会话的改名与删除（修复线 F29，`node-interop-manage.mjs`）：两边改名、软删除后另一边读到的标题、来源与列表一致，
//    首条输入相同。
// 14. Team Plan 与 z.ai 登录（工作线 F47、F49，`node-interop-team.mjs`，platform.md §9.4、§9.8）：rust-cli 登录存业务 JWT、
//    Node 独立 CLI 读到会再换一次；Node 登录存原始 OAuth token、rust-cli 读到直接用（被拒、要重新登录）；App 的
//    settingService 选团队 / 切回个人，rust-cli 不跟随；团队模型的会话 Node 能读（App 协议 schema）、能续写，
//    rust-cli 读回团队项目。
// 15. 个人套餐照 App 的登录（工作线 F50，`node-interop-app-login.mjs`，platform.md §5.2–§5.7）：只有 App 的登录时
//    rust-cli 能用个人套餐；App 与 rust-cli 对同一账号用同一份项目位置，谁先建另一方都复用；rust-cli 登录后 App 的
//    profile id 与换取能用；Node 独立 CLI 读 rust-cli 登录的凭据不再算已登录（以 App 为准的已知差异）。
// 16. 设备身份（工作线 F52，`node-interop-device.mjs`，platform.md §10、model.md §3.3）：Node 的 `ensureCliDeviceMid` 先生成，
//    rust-cli 的请求用同一个值、`metadata.user_id` 与 Node 算出的逐字节相同；rust-cli 先生成，Node 读到同一个值。
// 17. 模型请求头（工作线 F64，`node-interop-headers.mjs`，model.md §2.4）：Node 生成的对照夹具没有过时；真实的 `zcode -p`
//    发出的来源头与每请求头与 Node 的组装函数在同一组事实下给出的相同。
// 4. 界面语言（tui.md §7.1）：rust-cli 写 `~/.zcode/cli/config.json` 的 `ui.locale`（crates/platform/examples/
//    ui_locale.rs），Node 的 `loadFileConfig` 读出同一个值、其余字段不变；Node 的 `updateUiLocaleInFileConfig`
//    改写后 rust-cli 读出；同一份输入两边写出的文本逐字节相同。
//
// 用法：node scripts/rust-cli/node-interop-check.mjs [--node-root <装好依赖的检出>]
// 没有装依赖的检出时给出提示并以 2 退出；比对失败以 1 退出。
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual } from "node:util";
import { checkCompat, checkProviderImport, checkStoragePaths } from "./node-interop-compat.mjs";
import { checkFork } from "./node-interop-fork.mjs";
import { checkManage } from "./node-interop-manage.mjs";
import { nodeHistoryReader, rustText } from "./node-interop-history.mjs";
import {
  checkDefaultModel,
  checkMcpEnabled,
  checkUiLocale,
  checkUiTheme,
} from "./node-interop-settings.mjs";
import { checkInitialModel } from "./node-interop-model.mjs";
import { checkRace } from "./node-interop-race.mjs";
import { checkAppLogin } from "./node-interop-app-login.mjs";
import { checkDevice } from "./node-interop-device.mjs";
import { checkHeaders } from "./node-interop-headers.mjs";
import { checkTeam } from "./node-interop-team.mjs";
import { buildRuntimeBinaries, checkRuntime } from "./node-interop-runtime.mjs";
import { checkUsage } from "./node-interop-usage.mjs";
import { ensureTypeScript, importNode, nodeRootFromArgs, repoRoot } from "./node-root.mjs";

const { nodeRoot, args } = nodeRootFromArgs(process.argv.slice(2));
ensureTypeScript(import.meta.filename, nodeRoot, args);

const manifest = join(repoRoot, "packages/rust-cli/Cargo.toml");
// 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml（工作线 F46）。
const cargoAt = { cwd: dirname(manifest), stdio: "inherit" };
execFileSync(
  "cargo",
  [
    "build",
    "-q",
    "--manifest-path",
    manifest,
    "-p",
    "rust-cli-node-store",
    "--example",
    "node_interop",
  ],
  cargoAt,
);
execFileSync(
  "cargo",
  ["build", "-q", "--manifest-path", manifest, "-p", "rust-cli-platform", "--example", "ui_locale"],
  cargoAt,
);
execFileSync(
  "cargo",
  ["build", "-q", "--manifest-path", manifest, "-p", "rust-cli-platform", "--example", "ui_prefs"],
  cargoAt,
);
const example = (name) =>
  join(
    repoRoot,
    "packages/rust-cli/target/debug/examples",
    process.platform === "win32" ? `${name}.exe` : name,
  );
const binary = example("node_interop");
const uiLocale = (...command) =>
  JSON.parse(execFileSync(example("ui_locale"), command, { encoding: "utf8" }));
const uiPrefs = (...command) =>
  JSON.parse(execFileSync(example("ui_prefs"), command, { encoding: "utf8" }));
const rust = (...command) => JSON.parse(execFileSync(binary, command, { encoding: "utf8" }));

const { SqliteSessionStore } = await importNode(
  nodeRoot,
  "adapters/src/storage/session-store/sqlite-session-store.ts",
);
const { createNodeToolArtifactStore } = await importNode(nodeRoot, "adapters/src/storage/index.ts");
const { readSessionModelSelection } = await importNode(
  nodeRoot,
  "bootstrap/src/app/session-store.ts",
);
const { hydrateMessageHistoryFromSession } = await importNode(
  nodeRoot,
  "core/src/agent/session-history-hydrator.ts",
);
const { createMessageHistory } = await importNode(nodeRoot, "core/src/agent/message-history.ts");
const persistence = await importNode(nodeRoot, "core/src/runtime/methods/message-persistence.ts");
const { persistPendingToolPart } = await importNode(
  nodeRoot,
  "core/src/runtime/methods/tool-part-persistence.ts",
);
const { buildExecutionStateEntry } = await importNode(
  nodeRoot,
  "core/src/runtime/execution-state.ts",
);
const { toTokenUsageInfo } = await importNode(nodeRoot, "core/src/runtime/helpers/tool-result.ts");
const { projectIdFromDirectory, titleFromInput } = await importNode(
  nodeRoot,
  "core/src/runtime/helpers/project.ts",
);
const { createMessageId, createPartId, createTurnId } = await importNode(
  nodeRoot,
  "contracts/src/interfaces/shared.ts",
);
const { parseCompactBoundaryPayload } = await importNode(
  nodeRoot,
  "contracts/src/compact/index.ts",
);
const { parseCheckpointCreatedPayload, parseWorkspaceCheckpointArtifact } = await importNode(
  nodeRoot,
  "contracts/src/rewind/index.ts",
);
const { buildColdFileChangeSummaries } = await importNode(
  nodeRoot,
  "bootstrap/src/zcode-protocol-v4/cold-file-change-summaries.ts",
);
// Node 读执行状态用的 schema（core `resume.ts` 从 @zcode/shared 导入）。工作线 F46：来源分支上它被老 rust 的
// Host 对接移到 runtime-contracts/ 下，staging 上在 packages/shared/src/execution-state.ts，内容相同。
const { executionStateSchema } = await import(
  pathToFileURL(join(nodeRoot, "packages/shared/src/execution-state.ts")).href
);

const failures = [];
function expectEqual(name, node, rustValue) {
  if (isDeepStrictEqual(node, rustValue)) return;
  failures.push(`${name}\n  node: ${JSON.stringify(node)}\n  rust: ${JSON.stringify(rustValue)}`);
}

// 历史的归一与 Node 冷读取在 node-interop-history.mjs（第十一段在另一个进程里也用）。
const nodeHistory = nodeHistoryReader({ hydrateMessageHistoryFromSession, createMessageHistory });

async function executionState(store, sessionID) {
  const entries = await store.sessionEntries({ sessionID, type: "runtime/execution_state" });
  return executionStateSchema.parse(entries.at(-1)?.data);
}

const dir = await mkdtemp(join(tmpdir(), "rust-cli-node-interop-"));
try {
  const written = rust("write", dir);
  const dbPath = join(dir, "cli/db/db.sqlite");
  // Node 的启动迁移必须接受 rust-cli 建的库（账本、校验和一致，不重跑迁移）。
  const store = await SqliteSessionStore.openStartup({ dbPath });
  const artifactStore = createNodeToolArtifactStore({
    imageCacheRootDir: join(dir, "cli/image-cache"),
    pdfCacheRootDir: join(dir, "cli/pdf-cache"),
    rootDir: join(dir, "cli/artifacts"),
    videoCacheRootDir: join(dir, "cli/video-cache"),
  });
  try {
    const listed = (await store.listSessions({ directory: written.workspace, roots: true })).map(
      (s) => s.id,
    );
    expectEqual(
      "Node lists the rust-cli sessions",
      [...written.sessions].sort(),
      [...listed].sort(),
    );
    const rustListed = rust("list", dir, written.workspace).map((s) => s.id);
    expectEqual("both sides list the same order", listed, rustListed);
    // dontAsk 在 Node 里记为 build（rust-cli 另存自己的模式名）；acceptEdits 是 edit。
    const modes = [
      ["dontAsk", { mode: "build", planEnabled: false }],
      ["acceptEdits", { mode: "edit", planEnabled: false }],
      ["acceptEdits", { mode: "edit", planEnabled: false }],
      ["acceptEdits", { mode: "edit", planEnabled: false }],
      ["acceptEdits", { mode: "edit", planEnabled: false }],
    ];
    for (const [index, sessionID] of written.sessions.entries()) {
      const session = await store.getSession(sessionID);
      if (!session) throw new Error(`Node cannot read session ${sessionID}`);
      expectEqual(
        `${sessionID} model selection`,
        await readSessionModelSelection(store, sessionID),
        {
          providerId: "p",
          modelId: "m",
          options: { reasoningLevel: "high" },
        },
      );
      const [mode, execution] = modes[index];
      expectEqual(
        `${sessionID} execution state`,
        await executionState(store, sessionID),
        execution,
      );
      const node = await nodeHistory(store, artifactStore, session, sessionID);
      const loaded = rust("read", dir, sessionID);
      expectEqual(`${sessionID} history`, node, rustText(loaded.messages));
      // 标题与 Node `titleFromInput` 按首条真实输入算出的值比（第一段是生成的标题）；原来只比两边读同一行。
      const first = (await store.messages({ sessionID })).find(
        (m) => m.info.semantics?.origin === "real_user",
      );
      const typed = first.parts.find((part) => part.type === "text").text;
      const expectedTitle = index === 0 ? written.generatedTitle : titleFromInput(typed);
      expectEqual(
        `${sessionID} title`,
        [session.title, loaded.meta.title],
        [expectedTitle, expectedTitle],
      );
      expectEqual(`${sessionID} mode`, mode, loaded.mode);
    }

    // Read 整读 PDF 的结果（tools-files.md §1.5、node-store.md §4.6，修复线 F24）：tool part 里是 `application/pdf`
    // 的 data URL file part，Node 冷恢复成 PDF 的 file 块（与 rust-cli 读回的 document 块相同，上面的历史比较已含）。
    const pdfSession = written.sessions[2];
    const pdfPart = (await store.messages({ sessionID: pdfSession }))
      .flatMap((m) => m.parts)
      .find((part) => part.type === "tool" && part.callID === "toolu_5");
    expectEqual(
      "the PDF result is a Node file part",
      pdfPart?.state?.attachments?.map(({ type, mime, url }) => ({ type, mime, url })),
      [
        {
          type: "file",
          mime: "application/pdf",
          url: "data:application/pdf;base64,JVBERi0xLjQKJSVFT0YK",
        },
      ],
    );
    const pdfHistory = await nodeHistory(
      store,
      artifactStore,
      await store.getSession(pdfSession),
      pdfSession,
    );
    expectEqual(
      "Node reads the PDF result as a PDF file block",
      pdfHistory.find((item) => item.role === "tool" && item.id === "toolu_5")?.content,
      [
        { type: "text", text: "PDF file read: /w/interop/doc.pdf (14 bytes)" },
        {
          type: "document",
          mediaType: "application/pdf",
          dataUrl: "data:application/pdf;base64,JVBERi0xLjQKJSVFT0YK",
        },
      ],
    );

    // 生成的会话标题（titles.md §4）：Node 读出标题与 `generated` 来源。
    const titled = await store.getSession(written.sessions[0]);
    expectEqual("generated title", titled.title, written.generatedTitle);
    expectEqual("generated title source", titled.titleSource, "generated");

    // 本机项目规则集（shell-permissions.md §5）：Node 读回本工程写的“总是允许”规则；Node 再加一条 deny，
    // 本工程读回。
    const projectID = projectIdFromDirectory(written.workspace);
    const ruleset = await store.getProjectPermission(projectID);
    expectEqual("Node reads the rust-cli project ruleset", ruleset, {
      version: 1,
      allow: [{ toolName: "Bash", ruleContent: "git status:*" }, { toolName: "Read" }],
    });
    await store.saveProjectPermission({
      projectID,
      permission: { ...ruleset, deny: [{ toolName: "WebFetch" }] },
    });
    expectEqual("rust-cli reads the Node project ruleset", rust("rules", dir, written.workspace), {
      allow: [{ tool: "Bash", content: "git status:*" }, { tool: "Read" }],
      deny: [{ tool: "WebFetch" }],
    });
    // `/permissions` 的加与删（tui-manage.md §1.1）：rust-cli 加一条 ask、删一条 allow，Node 读出同样的规则集与
    // 键序；Node 再改，rust-cli 读回。
    const change = (value) => rust("rules-change", dir, written.workspace, JSON.stringify(value));
    change({ behavior: "ask", add: [{ tool: "Bash", content: "rm *" }] });
    change({ behavior: "allow", remove: [{ tool: "Read" }] });
    const changed = await store.getProjectPermission(projectID);
    expectEqual("Node reads the rules rust-cli added and removed", changed, {
      version: 1,
      allow: [{ toolName: "Bash", ruleContent: "git status:*" }],
      deny: [{ toolName: "WebFetch" }],
      ask: [{ toolName: "Bash", ruleContent: "rm *" }],
    });
    expectEqual("the ruleset keeps Node's key order", Object.keys(changed), [
      "version",
      "allow",
      "deny",
      "ask",
    ]);
    await store.saveProjectPermission({
      projectID,
      permission: { ...changed, ask: [], deny: [...changed.deny, { toolName: "Write" }] },
    });
    expectEqual(
      "rust-cli reads the ruleset Node changed after /permissions",
      rust("rules", dir, written.workspace),
      {
        allow: [{ tool: "Bash", content: "git status:*" }],
        deny: [{ tool: "WebFetch" }, { tool: "Write" }],
      },
    );

    // 项目的持久模式（config.md §2.2）：Node 读本工程写的值（acceptEdits → edit，dontAsk → build，guarded），本工程
    // 读 Node 写的值（guarded（修复线 F20 起不再读成 bypassPermissions），plan，Node 整份改写后不再有本工程的模式名）。
    expectEqual(
      "Node reads the rust-cli project mode",
      store.getProjectPermissionMode(projectID),
      "edit",
    );
    expectEqual(
      "rust-cli reads its project mode",
      rust("mode", dir, written.workspace),
      "acceptEdits",
    );
    rust("mode-set", dir, written.sessions[0], "dontAsk");
    expectEqual("Node reads dontAsk as build", store.getProjectPermissionMode(projectID), "build");
    expectEqual("rust-cli tells dontAsk apart", rust("mode", dir, written.workspace), "dontAsk");
    rust("mode-set", dir, written.sessions[0], "guarded");
    expectEqual("Node reads guarded", store.getProjectPermissionMode(projectID), "guarded");
    expectEqual("rust-cli reads its guarded", rust("mode", dir, written.workspace), "guarded");
    for (const [node, expected] of [
      ["guarded", "guarded"],
      ["plan", "plan"],
      ["build", "default"],
      ["yolo", "bypassPermissions"],
    ]) {
      store.saveProjectPermissionMode({ mode: node, projectID });
      expectEqual(
        `rust-cli reads the Node project mode ${node}`,
        rust("mode", dir, written.workspace),
        expected,
      );
    }

    // 执行状态双向（修复线 F20，node-store.md §4.3）：本工程写“guarded + plan 开”与关掉 plan，Node 读出同样的
    // `{mode, planEnabled}`；Node 写“guarded + plan 开”，本工程读出 guarded 与 plan 开。
    const execID = written.sessions[1];
    rust("mode-exec", dir, execID, "guarded", "true");
    expectEqual("Node reads guarded with plan on", await executionState(store, execID), {
      mode: "guarded",
      planEnabled: true,
    });
    expectEqual(
      "the session row says plan while plan is on",
      (await store.getSession(execID)).permission?.mode,
      "plan",
    );
    rust("mode-exec", dir, execID, "dontAsk", "true");
    expectEqual("Node reads dontAsk with plan on as build", await executionState(store, execID), {
      mode: "build",
      planEnabled: true,
    });
    const dontAsk = rust("read", dir, execID);
    expectEqual(
      "rust-cli keeps dontAsk under plan",
      [dontAsk.mode, dontAsk.plan],
      ["dontAsk", true],
    );
    rust("mode-exec", dir, execID, "guarded", "false");
    expectEqual("Node reads guarded after plan is off", await executionState(store, execID), {
      mode: "guarded",
      planEnabled: false,
    });
    await store.saveSessionEntry(
      buildExecutionStateEntry(execID, { mode: "guarded", planEnabled: true }),
    );
    const execLoaded = rust("read", dir, execID);
    expectEqual(
      "rust-cli reads Node's guarded with plan on",
      [execLoaded.mode, execLoaded.plan],
      ["guarded", true],
    );
    await store.saveSessionEntry(
      buildExecutionStateEntry(execID, { mode: "yolo", planEnabled: false }),
    );
    const yoloLoaded = rust("read", dir, execID);
    expectEqual(
      "rust-cli reads Node's yolo with plan off",
      [yoloLoaded.mode, yoloLoaded.plan ?? false],
      ["bypassPermissions", false],
    );

    // 第四段：压缩边界过 Node 的 strict schema；Node 的上下文从摘要开始，保留尾段（工具调用与结果）
    // 接在摘要之后（compact.md §6）。
    const compactedID = written.sessions[3];
    const compactions = (await store.messages({ sessionID: compactedID }))
      .flatMap((m) => m.parts)
      .filter((part) => part.type === "compaction");
    expectEqual("one compaction part", compactions.length, 1);
    const boundary = parseCompactBoundaryPayload(compactions[0].compactBoundary);
    const compactedLoaded = rust("read", dir, compactedID);
    const first = compactedLoaded.messages[0];
    expectEqual(
      "rust-cli boundary first",
      first.type === "system" && first.subtype,
      "compact_boundary",
    );
    expectEqual("boundary id", boundary.boundaryId, first.id);
    expectEqual("boundary summary", boundary.summaryMessageIds, [first.summary]);
    expectEqual("preserved tail maps to the assistant row", boundary.preservedSegment, {
      headMessageId: first.preserved.head,
      anchorMessageId: first.summary,
      tailMessageId: first.preserved.head,
    });
    expectEqual(
      "compacted context",
      rustText(compactedLoaded.messages).map((item) => item.role),
      ["user", "assistant", "tool", "user", "user", "assistant"],
    );

    // 第五段：checkpoint 过 Node 的 strict schema，artifact 能读回，App 的冷改动摘要算得出；
    // 回退游标是 Node 的行（工具结果没有行），活动分支与本工程冷读取一致（session-lifecycle.md §4、§5）。
    const checkpointedID = written.sessions[4];
    const checkpointEntries = await store.sessionEntries({
      sessionID: checkpointedID,
      type: "runtime/workspace_checkpoint",
    });
    expectEqual("one workspace checkpoint", checkpointEntries.length, 1);
    const checkpointData = checkpointEntries[0].data;
    const checkpointPayload = parseCheckpointCreatedPayload(checkpointData.payload);
    expectEqual(
      "checkpoint event fields",
      [
        typeof checkpointData.eventId,
        typeof checkpointData.sequenceNumber,
        typeof checkpointData.traceId,
      ],
      ["string", "number", "string"],
    );
    const checkpointArtifact = parseWorkspaceCheckpointArtifact(
      JSON.parse(
        (await artifactStore.readToolResultArtifact({ uri: checkpointPayload.snapshotRef }))
          .content,
      ),
    );
    expectEqual("checkpoint file", checkpointArtifact.files[0].path, written.checkpointFile);
    const summaries = await buildColdFileChangeSummaries({
      events: [
        {
          type: "checkpoint_created",
          payload: checkpointPayload,
          turnId: checkpointData.turnId,
        },
      ],
      messageIds: [checkpointPayload.messageId],
      readArtifact: async (uri) => (await artifactStore.readToolResultArtifact({ uri })).content,
    });
    const summary = summaries.get(checkpointPayload.messageId);
    expectEqual(
      "App file change summary",
      summary && [summary.files, summary.additions, summary.deletions],
      [1, 1, 1],
    );
    const checkpointedSession = await store.getSession(checkpointedID);
    const checkpointedLoaded = rust("read", dir, checkpointedID);
    expectEqual(
      "rust-cli keeps the checkpoint",
      checkpointedLoaded.checkpoints.map((c) => c.id),
      [checkpointPayload.checkpointId],
    );
    const rows = checkpointedLoaded.messages
      .filter((m) => !(m.type === "user" && m.content?.[0]?.type === "tool_result"))
      .map((m) => m.id);
    expectEqual("revert keeps the Node rows", checkpointedSession.revert.keptMessageIDs, rows);
    expectEqual("revert generation", checkpointedSession.revert.branchGeneration, 1);
    expectEqual(
      "revert target is the second prompt",
      checkpointedSession.revert.kind,
      "conversation_rewind",
    );

    // Node 的写入代码要的 runtime 成员（`AgentRuntimeInternal` 的子集）。
    async function nodeRuntime(sessionID) {
      const selection = await readSessionModelSelection(store, sessionID);
      const execution = await executionState(store, sessionID);
      const runtime = {
        sessionId: sessionID,
        sessionStore: store,
        config: { mode: execution.mode },
        getTools: () => [],
        getSessionModelSelection: () => selection,
        getPlanEnabled: () => execution.planEnabled,
        workingDirectory: written.workspace,
        workspaceRoot: written.workspace,
      };
      runtime.persistMessage = persistence.persistMessage.bind(runtime);
      runtime.persistPart = persistence.persistPart.bind(runtime);
      return { runtime, selection };
    }

    // Node 续写第一个会话：一轮 user / assistant（Node 自己的写入代码），再把执行状态改成 yolo。
    const sessionID = written.sessions[0];
    const { runtime, selection } = await nodeRuntime(sessionID);
    const trace = { traceId: sessionID, turnId: createTurnId() };
    const userID = createMessageId();
    await persistence.persistUserPrompt.call(runtime, userID, "Node continues", undefined, trace, {
      sourceCommandId: "node_cmd",
    });
    const assistantID = createMessageId();
    const created = Date.now();
    const tokens = toTokenUsageInfo({ inputTokens: 30, outputTokens: 3, totalTokens: 33 });
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
      { type: "text", text: "Node answer", time: { start: created, end: Date.now() } },
      { type: "step-finish", reason: "stop", cost: 0, tokens },
    ]) {
      await runtime.persistPart({ id: createPartId(), sessionID, messageID: assistantID, ...part });
    }
    await store.saveSessionEntry(
      buildExecutionStateEntry(sessionID, { mode: "yolo", planEnabled: false }),
    );
    const session = await store.getSession(sessionID);
    const node = await nodeHistory(store, artifactStore, session, sessionID);
    const loaded = rust("read", dir, sessionID);
    expectEqual("history after Node continued", node, rustText(loaded.messages));
    expectEqual(
      "last two messages after Node continued",
      loaded.messages.slice(-2).map((m) => m.id),
      [userID, assistantID],
    );
    expectEqual("mode after Node switched to yolo", loaded.mode, "bypassPermissions");

    // 工具会话：rust-cli 冷读取出未结的操作（op_4 没有 assistant）与没回答的计划审批。
    const toolsID = written.sessions[2];
    const openWork = (read) => ({
      operations: read.open_operations ?? [],
      interactions: read.pending_interactions ?? [],
    });
    const expectedWork = {
      operations: [
        {
          operation: "op_4",
          // 意图存 fingerprint 的摘要（node-store.md §4.7，修复线 F6）。
          fingerprint: `sha256:${createHash("sha256")
            .update(JSON.stringify({ tool: "Bash", input: { command: "make" } }))
            .digest("hex")}`,
        },
      ],
      interactions: [
        {
          interaction: "ia_1",
          request: { kind: "plan_approval", tool_use: "toolu_9", plan: "Ship it" },
        },
      ],
    };
    expectEqual(
      "open work of the tool session",
      openWork(rust("read", dir, toolsID)),
      expectedWork,
    );

    // Node 续写工具会话：一轮带工具调用的回合（Node 的 pending tool part 与 settleToolResults 的
    // completed 形状），再给出最后的回答。
    const tools = await nodeRuntime(toolsID);
    const model = { providerId: tools.selection.providerId, modelId: tools.selection.modelId };
    const toolTrace = { traceId: toolsID, turnId: createTurnId() };
    const askID = createMessageId();
    await persistence.persistUserPrompt.call(
      tools.runtime,
      askID,
      "Node reads a file",
      undefined,
      toolTrace,
      { sourceCommandId: "node_cmd_tools" },
    );
    const stepID = createMessageId();
    const stepCreated = Date.now();
    const stepTokens = toTokenUsageInfo({ inputTokens: 40, outputTokens: 4, totalTokens: 44 });
    await persistence.persistAssistantMessage.call(
      tools.runtime,
      stepID,
      askID,
      stepCreated,
      { completed: Date.now(), finish: "tool-calls", tokens: stepTokens },
      toolTrace,
      model,
    );
    const toolSession = { sessionID: toolsID, messageID: stepID };
    await tools.runtime.persistPart({ id: createPartId(), ...toolSession, type: "step-start" });
    const toolPartID = createPartId();
    const toolCall = { id: "call_node_1", name: "Read", input: { file_path: "/w/interop/n.txt" } };
    await persistPendingToolPart(tools.runtime, {
      assistantMessageId: stepID,
      declarationIndex: 0,
      input: toolCall.input,
      partID: toolPartID,
      toolCall,
      traceContext: toolTrace,
      model,
    });
    await tools.runtime.persistPart({
      id: toolPartID,
      ...toolSession,
      type: "tool",
      callID: toolCall.id,
      declarationIndex: 0,
      tool: toolCall.name,
      metadata: {},
      state: {
        status: "completed",
        input: toolCall.input,
        output: "node file",
        title: toolCall.name,
        metadata: { schemaVersion: 1 },
        time: { start: stepCreated, end: Date.now() },
      },
    });
    await tools.runtime.persistPart({
      id: createPartId(),
      ...toolSession,
      type: "step-finish",
      reason: "tool-calls",
      cost: 0,
      tokens: stepTokens,
    });
    const finalID = createMessageId();
    const finalCreated = Date.now();
    await persistence.persistAssistantMessage.call(
      tools.runtime,
      finalID,
      askID,
      finalCreated,
      { completed: Date.now(), finish: "stop", tokens },
      toolTrace,
      model,
    );
    await tools.runtime.persistPart({
      id: createPartId(),
      sessionID: toolsID,
      messageID: finalID,
      type: "text",
      text: "Node read it.",
      time: { start: finalCreated, end: Date.now() },
    });
    const toolsSession = await store.getSession(toolsID);
    const toolsNode = await nodeHistory(store, artifactStore, toolsSession, toolsID);
    const toolsLoaded = rust("read", dir, toolsID);
    expectEqual("tool history after Node continued", toolsNode, rustText(toolsLoaded.messages));
    // Node 写的工具结果没有消息行：rust-cli 用 part id 作结果消息的 id。
    expectEqual(
      "last four messages after Node continued with a tool",
      toolsLoaded.messages.slice(-4).map((m) => m.id),
      [askID, stepID, toolPartID, finalID],
    );
    expectEqual("open work after Node continued", openWork(toolsLoaded), expectedWork);

    // 第七段：Node 的 `/fork`。
    await checkFork({
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
    });

    // 第九段：用量表（usage.md §9）。
    await checkUsage({
      nodeRoot,
      store,
      dir,
      rust,
      expectEqual,
      persistence,
      createMessageId,
      createTurnId,
      projectIdFromDirectory,
      workspace: written.workspace,
    });
    // 第十段：与 Node 共用数据的兼容（修复线 F2，node-interop-compat.mjs）。
    await checkCompat({
      nodeRoot,
      store,
      dir,
      rust,
      expectEqual,
      nodeHistory,
      rustText,
      artifactStore,
      written,
      // F37 / F38：提交前被中断的会话与 esc 撤回的会话由 Node 续写（同第三段的写入代码）。
      node: {
        nodeRuntime,
        persistence,
        createMessageId,
        createPartId,
        createTurnId,
        toTokenUsageInfo,
      },
    });
    // 第十三段：会话的改名与删除（修复线 F29，node-interop-manage.mjs）。
    await checkManage({ store, dir, rust, binary, expectEqual });
  } finally {
    store.close();
  }
  await checkProviderImport({ nodeRoot, example, home: join(dir, "provider-home"), expectEqual });
  await checkStoragePaths({ nodeRoot, repoRoot, base: join(dir, "storage"), expectEqual });
  const settings = { nodeRoot, uiLocale, uiPrefs, expectEqual };
  await checkUiLocale({ ...settings, home: join(dir, "locale-home") });
  await checkUiTheme({ ...settings, home: join(dir, "theme-home") });
  await checkDefaultModel({ ...settings, home: join(dir, "model-home") });
  // 第八段：新会话的默认模型按 Node `resolveInitialModelSelection`（workspace-trust.md §3）。
  await checkInitialModel({
    nodeRoot,
    repoRoot,
    example,
    home: join(dir, "initial-home"),
    expectEqual,
  });
  await checkMcpEnabled({ ...settings, home: join(dir, "mcp-home") });
  // 第十一段：Node 的真实运行时（子进程，临时 HOME）。
  const runtimeBinaries = buildRuntimeBinaries();
  await checkRuntime({
    nodeRoot,
    base: join(dir, "runtime"),
    expectEqual,
    binaries: runtimeBinaries,
  });
  // 第十二段：双进程并发写与迁移竞争。
  await checkRace({ nodeRoot, repoRoot, base: join(dir, "race"), expectEqual, example });
  // 第十四段：Team Plan 与 z.ai 登录（工作线 F47）。
  const team = await checkTeam({
    nodeRoot,
    repoRoot,
    base: join(dir, "team"),
    expectEqual,
    zcode: runtimeBinaries.zcode,
    rust,
  });
  // 第十五段：个人套餐照 App 的登录（工作线 F50）。
  const appLogin = await checkAppLogin({
    nodeRoot,
    repoRoot,
    base: join(dir, "app-login"),
    expectEqual,
    zcode: runtimeBinaries.zcode,
  });
  // 第十六段：设备身份（工作线 F52）。
  const device = await checkDevice({
    nodeRoot,
    base: join(dir, "device"),
    expectEqual,
    zcode: runtimeBinaries.zcode,
  });
  // 第十七段：模型请求头（工作线 F64）。
  const headers = await checkHeaders({
    nodeRoot,
    repoRoot,
    base: join(dir, "headers"),
    expectEqual,
    zcode: runtimeBinaries.zcode,
  });
  if (failures.length) {
    console.error(`node-interop: ${failures.length} mismatch(es)\n${failures.join("\n")}`);
    process.exitCode = 1;
  } else {
    console.log(
      `node-interop: OK — Node read ${written.sessions.length} rust-cli sessions identically ` +
        "(text, reasoning, attachments, tool calls and results), continued a text session and " +
        "a tool session, and rust-cli read Node's continuations back; the project permission " +
        "ruleset, the project permission mode and the execution state (guarded, planEnabled) round-trip both ways and Node reads the " +
        "generated title; ui.locale and ui.theme written by either side are read by the other, " +
        "with the same file text; the /effort and /model default model selection round-trips " +
        "both ways with the same file text, and both sides resolve the same initial model for " +
        "new sessions (Node resolveInitialModelSelection); /mcp enable and disable write the enabled field Node reads; " +
        "a rust-cli fork is read and continued by Node, and a Node fork is read by rust-cli; " +
        "the usage rows rust-cli writes (the session title row under the Node name session_title, " +
        "outside turn_usage as Node's recordModelUsageFact and recordTurnUsageFact write it) match " +
        "Node's rows column by column and Node's usage " +
        "queries read them, and rust-cli's /usage queries read a Node session and its footer " +
        "TPS and cache hit rate match Node's debug panel over the same requests; every message " +
        "passes the App protocol schema, interrupted steps, a turn interrupted before its request " +
        "and an input restored with esc (read and continued by Node), model ids, last outcomes, bash and " +
        "skill inputs, subagent links, mixed checkpoints, group session rules, todos, the legacy " +
        "provider import and the storage settings follow Node; Node's real runtime continues, reverts, " +
        "file-rewinds and compacts rust-cli sessions (and the other way round) with the same model history, " +
        "concurrent writers and migrations from both sides neither lose nor corrupt data, " +
        "and session renames and soft deletes made by either side are read the same way by the other; " +
        "Team Plan: the connection chosen through the App's settingService does not switch rust-cli " +
        "`zcode -p`, `--model <team>[<project>]` uses that team project's token, Node reads (App protocol " +
        "schema) and continues a team-model session and rust-cli reads the team project back; rust-cli stores the " +
        `z.ai business JWT, which Node's standalone CLI exchanges again (${team.nodeReExchangesTheBusinessJwt}), ` +
        `and rust-cli sends the raw OAuth token stored by Node as the business JWT (${team.rustUsesTheRawTokenDirectly}; ` +
        "rejected, log in again); the individual plan follows the App login: with only the App's " +
        "credentials rust-cli uses it after the App's subscription check, the App and rust-cli share one " +
        `project location either way (App reuses rust-cli's: ${appLogin.appReusesRustLocation}, rust-cli ` +
        `reuses the App's: ${appLogin.rustReusesAppLocation}), the App reads the profile id rust-cli's login ` +
        `writes, and Node's standalone CLI no longer sees a rust-cli login (${appLogin.nodeSeesRustLogin}; ` +
        "accounts follow the App); the device id is shared: rust-cli sends the deviceMid Node's ensureCliDeviceMid " +
        `wrote with the same metadata.user_id text (${device.nodeFirst}) and Node reads the one rust-cli wrote ` +
        `(${device.rustFirst}); the model request headers follow Node: the golden fixtures Node's header ` +
        "builders generate are up to date, and a real `zcode -p` sends the same source headers " +
        `(ZCode/${headers.version}, ${headers.locale}, ${headers.zone}) and per-request headers as Node builds ` +
        "for the same facts.",
    );
  }
} finally {
  await rm(dir, { recursive: true, force: true });
}
