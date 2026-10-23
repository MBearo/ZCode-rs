#!/usr/bin/env node
// rust-cli 与 Node CLI 的互操作检查第十一段：Node 的真实运行时（docs/specs/rust-cli/node-store.md §6 第 11 步）。
//
// 前十段只调用 Node 的写入函数；这一段起 Node 的真实运行时（bootstrap 的 `createZCodeApp`，真实回合、回退、文件
// 回退、压缩），与真实的 `zcode -p` 及 `node_kernel`（rust-cli 的 `/compact`、`/rewind`、`/branch`）轮流写同一个
// 库、同一份 provider 配置，模型是本地替身（scripts/rust-cli/fixture/model.mjs）。场景见
// node-interop-runtime-scenarios.mjs，驱动见 node-interop-runtime-harness.mjs。
//
// 运行方式：Node 的运行时会按 `homedir()` 找配置、日志与缺省库，所以场景在子进程里跑，子进程的环境只有临时
// HOME、PATH 与 TMPDIR（不碰真实的 ~/.zcode）。父进程（node-interop-check.mjs 或单独运行）构建二进制、建临时
// 目录、启动子进程、读回结果。
//
// 单独运行：node scripts/rust-cli/node-interop-runtime.mjs [--node-root <装好依赖的检出>] [--only <场景,…>]
import { execFileSync, spawn } from "node:child_process";
import { mkdir, mkdtemp, readFile, realpath, rm, writeFile } from "node:fs/promises";
import { homedir, tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { pathToFileURL } from "node:url";
import { isDeepStrictEqual } from "node:util";
import { BINARY_NAME, binaryFile } from "./binary.mjs";
import { nodeHistoryReader } from "./node-interop-history.mjs";
import {
  nodeSide,
  rustSide,
  startModel,
  writeProviderConfig,
} from "./node-interop-runtime-harness.mjs";
import { runScenarios } from "./node-interop-runtime-scenarios.mjs";
import { ensureTypeScript, importNode, nodeRootFromArgs, repoRoot } from "./node-root.mjs";

const RESULT = "runtime-result.json";
const exe = (name) => (process.platform === "win32" ? `${name}.exe` : name);

/** 构建这一段要的 rust 二进制：`zcode-rs`、`node_kernel`、`node_interop`。 */
export function buildRuntimeBinaries(root = repoRoot) {
  const manifest = join(root, "packages/rust-cli/Cargo.toml");
  const cargo = (...args) =>
    // 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml（工作线 F46）。
    execFileSync("cargo", ["build", "-q", "--manifest-path", manifest, ...args], {
      cwd: dirname(manifest),
      stdio: "inherit",
    });
  cargo("-p", "rust-cli", "--bin", BINARY_NAME, "--example", "node_kernel");
  cargo("-p", "rust-cli-node-store", "--example", "node_interop");
  const target = join(root, "packages/rust-cli/target/debug");
  return {
    zcode: join(target, binaryFile()),
    kernel: join(target, "examples", exe("node_kernel")),
    interop: join(target, "examples", exe("node_interop")),
  };
}

/**
 * 父进程：在 `base` 下建临时 HOME 与工作区，起子进程跑场景，把每个不一致交给 `expectEqual`。
 * 返回子进程报的耗时。
 */
export async function checkRuntime({ nodeRoot, base, expectEqual, binaries, only = [] }) {
  const home = join(base, "home");
  await mkdir(home, { recursive: true });
  const loader = join(nodeRoot, "node_modules/tsx/dist/loader.mjs");
  const env = {
    HOME: home,
    USERPROFILE: home,
    PATH: process.env.PATH,
    TMPDIR: process.env.TMPDIR ?? tmpdir(),
    RUST_CLI_NODE_TSX: "1",
  };
  const args = [
    "--no-warnings",
    "--import",
    pathToFileURL(loader).href,
    import.meta.filename,
    "--child",
    "--node-root",
    nodeRoot,
    "--base",
    base,
    "--bin",
    JSON.stringify(binaries),
    ...(only.length ? ["--only", only.join(",")] : []),
  ];
  const code = await new Promise((resolveRun, rejectRun) => {
    const child = spawn(process.execPath, args, { env, stdio: "inherit" });
    child.on("error", rejectRun);
    child.on("exit", resolveRun);
  });
  let result;
  try {
    result = JSON.parse(await readFile(join(base, RESULT), "utf8"));
  } catch (error) {
    expectEqual(
      "node runtime section: the child wrote its result",
      `exit ${code}: ${error}`,
      "result",
    );
    return {};
  }
  for (const { name, got, want } of result.mismatches) expectEqual(name, got, want);
  return result.timings;
}

/** 子进程：临时 HOME 下跑场景，结果写到 `<base>/runtime-result.json`。 */
async function child(nodeRoot, base, binaries, only) {
  // 安全：Node 的缺省库、日志与配置都按 homedir()；必须是临时目录。
  if (!resolve(homedir()).startsWith(resolve(base))) throw new Error(`HOME is not under ${base}`);
  await mkdir(join(base, "work", ".git"), { recursive: true });
  // 会话行的目录是进程的 cwd（macOS 的临时目录经符号链接解析成 /private/…）。
  const work = await realpath(join(base, "work"));
  await writeFile(join(work, "notes.txt"), "line one\nline two\n");
  const storage = join(base, "storage");
  const layout = {
    base,
    home: homedir(),
    work,
    storage,
    db: join(storage, "cli/db/db.sqlite"),
    env: {
      HOME: homedir(),
      USERPROFILE: homedir(),
      PATH: process.env.PATH,
      TMPDIR: process.env.TMPDIR ?? tmpdir(),
      SHELL: "/bin/bash",
      ZCODE_DATA_BASE_DIR: homedir(),
      ZCODE_STORAGE_DIR: storage,
      ZCODE_SESSION_DB_PATH: join(storage, "cli/db/db.sqlite"),
    },
  };
  const model = await startModel();
  const personalFile = await writeProviderConfig(layout.home, model.baseUrl);
  const mismatches = [];
  const check = (name, got, want) => {
    if (!isDeepStrictEqual(got, want)) mismatches.push({ name, got, want });
  };
  const node = await nodeSide({ nodeRoot, repoRoot, layout, personalFile });
  const { SqliteSessionStore } = await importNode(
    nodeRoot,
    "adapters/src/storage/session-store/sqlite-session-store.ts",
  );
  const { createNodeToolArtifactStore } = await importNode(
    nodeRoot,
    "adapters/src/storage/index.ts",
  );
  const { hydrateMessageHistoryFromSession } = await importNode(
    nodeRoot,
    "core/src/agent/session-history-hydrator.ts",
  );
  const { createMessageHistory } = await importNode(nodeRoot, "core/src/agent/message-history.ts");
  const { restoreWorkspaceCheckpointEntries } = await importNode(
    nodeRoot,
    "core/src/runtime/methods/workspace-checkpoint-persistence.ts",
  );
  const { createInMemorySessionEventStore } = await importNode(
    nodeRoot,
    "contracts/src/events/in-memory-session-event-store.ts",
  );
  const { buildColdFileChangeSummaries } = await importNode(
    nodeRoot,
    "bootstrap/src/zcode-protocol-v4/cold-file-change-summaries.ts",
  );
  // 先用 Node 建库（迁移），两边之后都只是打开。
  const store = await SqliteSessionStore.openStartup({ dbPath: layout.db });
  const artifactStore = createNodeToolArtifactStore({
    imageCacheRootDir: join(storage, "cli/image-cache"),
    pdfCacheRootDir: join(storage, "cli/pdf-cache"),
    rootDir: join(storage, "cli/artifacts"),
    videoCacheRootDir: join(storage, "cli/video-cache"),
  });
  /** App 的冷改动摘要（v4 bridge 的读法）：每条 user 消息 → `[路径, 增, 删]`。 */
  async function nodeChangeSummaries(sessionID) {
    const eventStore = createInMemorySessionEventStore();
    await restoreWorkspaceCheckpointEntries(
      { sessionId: sessionID, sessionStore: store, eventStore },
      { traceId: sessionID },
    );
    const summaries = await buildColdFileChangeSummaries({
      events: await eventStore.getEvents(sessionID),
      messageIds: (await store.messages({ sessionID })).map((message) => String(message.info.id)),
      readArtifact: async (uri) => (await artifactStore.readToolResultArtifact({ uri })).content,
    });
    return new Map(
      [...summaries].map(([id, summary]) => [
        id,
        summary.items.map((item) => [item.path, item.additions, item.deletions]),
      ]),
    );
  }
  /** 两边写的文件回退（`runtime/workspace_file_rewind`）撤销的回合：Node 的 `targetMessageId`。 */
  async function nodeFileRewinds(sessionID) {
    const entries = await store.sessionEntries({
      sessionID,
      type: "runtime/workspace_file_rewind",
    });
    return entries.map((entry) => entry.data?.payload?.targetMessageId);
  }
  const ctx = {
    nodeRoot,
    nodeFileRewinds,
    layout,
    model,
    node,
    rust: rustSide({ ...binaries, layout, baseUrl: model.baseUrl }),
    store,
    artifactStore,
    nodeHistory: nodeHistoryReader({ hydrateMessageHistoryFromSession, createMessageHistory }),
    nodeChangeSummaries,
    check,
  };
  let timings;
  try {
    timings = await runScenarios(ctx, only);
  } finally {
    store.close();
    node.dispose();
    await model.close();
  }
  timings.requests = model.records.length;
  await writeFile(join(base, RESULT), `${JSON.stringify({ mismatches, timings }, null, 2)}\n`);
}

const argv = process.argv.slice(2);
if (argv.includes("--child")) {
  const value = (flag) => argv[argv.indexOf(flag) + 1];
  const only = argv.includes("--only") ? value("--only").split(",") : [];
  await child(
    resolve(value("--node-root")),
    resolve(value("--base")),
    JSON.parse(value("--bin")),
    only,
  );
  // Node 的运行时可能留下后台计时器（遥测、轮询）；结果已写完，直接退出。
  process.exit(0);
} else if (import.meta.filename === resolve(process.argv[1] ?? "")) {
  // 单独运行：构建、建临时目录、跑、打印。
  const { nodeRoot, args } = nodeRootFromArgs(argv);
  ensureTypeScript(import.meta.filename, nodeRoot, args);
  const only = args.includes("--only") ? args[args.indexOf("--only") + 1].split(",") : [];
  const binaries = buildRuntimeBinaries();
  const base = await realpath(await mkdtemp(join(tmpdir(), "rust-cli-node-runtime-")));
  const failures = [];
  const expectEqual = (name, got, want) => {
    if (!isDeepStrictEqual(got, want))
      failures.push(`${name}\n  got:  ${JSON.stringify(got)}\n  want: ${JSON.stringify(want)}`);
  };
  try {
    const started = Date.now();
    const timings = await checkRuntime({ nodeRoot, base, expectEqual, binaries, only });
    console.log(`node runtime section: ${Date.now() - started} ms ${JSON.stringify(timings)}`);
    if (failures.length) {
      console.error(
        `node-interop-runtime: ${failures.length} mismatch(es)\n${failures.join("\n")}`,
      );
      process.exitCode = 1;
    } else console.log("node-interop-runtime: OK");
  } finally {
    if (!process.env.KEEP_RUNTIME_BASE) await rm(base, { recursive: true, force: true });
    else console.log(`kept ${base}`);
  }
}
