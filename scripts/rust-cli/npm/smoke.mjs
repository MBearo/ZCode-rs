// 发版前的冒烟（.github/workflows/rust-cli-release.yml 的 build 任务，三个平台都跑）：在临时目录里离线安装刚打好的
// 入口包与本平台的包，然后
// 1. `zcode --version` 是本次的版本；
// 2. 真正打开会话库：临时 HOME 下运行 `zcode -p -r <固定的 UUID> x`。新库里没有这个会话，CLI 建库、跑迁移、按 ID 查会话，报
//    “No conversation found with session ID: <UUID>” 并退出 1；这条路径不联网、不要求登录。之后读库里的 `schema_migration`，
//    （以前用 `-p -c x`：现在 `-p --continue` 找不到会话时会新建会话并发模型请求，不再能离线建库。）
//    每个迁移的校验和都要等于 Node 账本里的值。
// 第 2 步防的问题：Windows 的检出曾把编入的迁移 SQL 变成 CRLF，校验和与 Node 不同，用户原有的库打不开；
// `--version` 在打开存储之前就返回，查不出来。
// 3. 平台包里是 `bin/zcode-rs`（Windows 是 `bin/zcode.exe`；命令名仍是 `zcode`）与 THIRD-PARTY-NOTICES.txt；
// 4. 内嵌的搜索工具编进去了：原生二进制以 `ARGV0=rg` / `ugrep` / `bfs`（Windows 没有 bfs）运行时打印工具自己的版本；
//    Linux 上 ugrep 的 C++ 运行库是静态链接的（不需要 libstdc++.so）。
import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { inspectElf } from "../linux/elf.mjs";
import { metadata, platforms } from "./package.mjs";
import { npm } from "./npm.mjs";

const { executableFile } = createRequire(import.meta.url)("./package/bin/zcode.cjs");

/** 期望的校验和：rust 单测里与 Node 账本逐项核对过的表（`EXPECTED`）。 */
export const LEDGER_TABLE = resolve(
  import.meta.dirname,
  "../../../packages/rust-cli/crates/node-store/src/repo/migrations_tests.rs",
);
/** 会话库在 HOME 下的位置（Node 与 rust 的缺省值）。 */
export const DATABASE = [".zcode", "cli", "db", "db.sqlite"];
/** `-p -c` 在没有会话时的报错原文。 */
/** 冒烟时续写的会话 ID：新库里一定没有它。 */
export const MISSING_SESSION = "2d480878-38af-4bae-a54f-096e2fb0f4d7";
export const NO_CONVERSATION = `No conversation found with session ID: ${MISSING_SESSION}`;

/** 内嵌的搜索工具：`ARGV0` 的值与 `--version` 第一行的开头。Windows 没有 bfs。 */
export function embeddedTools(os) {
  const tools = [
    { name: "rg", prefix: "ripgrep " },
    { name: "ugrep", prefix: "ugrep " },
    ...(os === "win32" ? [] : [{ name: "bfs", prefix: "bfs " }]),
  ];
  return tools.map((tool) => ({ ...tool, matches: (stdout) => stdout.startsWith(tool.prefix) }));
}

/** 动态依赖里的 C++ 运行库（应当为空：linux-gnu 静态链接 libstdc++）。 */
export function dynamicCxxRuntime(needed) {
  return needed.filter((name) => /^lib(stdc|c)\+\+\.so/u.test(name));
}

/** 装好的平台包缺什么：本平台的可执行文件（`bin/zcode-rs`，Windows `bin/zcode.exe`）与第三方许可证声明。 */
export async function installedPlatformProblems(folder, os) {
  const isFile = async (path) => (await stat(path).catch(() => null))?.isFile() ?? false;
  const problems = [];
  const executable = `bin/${executableFile(os)}`;
  if (!(await isFile(join(folder, executable)))) problems.push(`${executable} is missing`);
  if (!(await isFile(join(folder, "THIRD-PARTY-NOTICES.txt")))) {
    problems.push("THIRD-PARTY-NOTICES.txt is missing");
  }
  return problems;
}

/** 从 `migrations_tests.rs` 的源码取出 `EXPECTED`：迁移 id → 校验和。表的长度与条目数不一致时报错。 */
export function expectedLedger(source) {
  const table = source.match(/const EXPECTED: \[\(&str, &str\); (\d+)\] = \[([\s\S]*?)\r?\n\];/u);
  if (!table) throw new Error("migrations_tests.rs has no EXPECTED table");
  const entries = [...table[2].matchAll(/\(\s*"([0-9a-z_]+)",\s*"([0-9a-f]{64})",?\s*\)/gu)];
  if (entries.length !== Number(table[1])) {
    throw new Error(`EXPECTED declares ${table[1]} migrations but lists ${entries.length}`);
  }
  return new Map(entries.map(([, id, checksum]) => [id, checksum]));
}

/** 库里的账本与期望值比较：缺的、校验和不同的、多出来的，各一行；一致时为空。 */
export function ledgerProblems(rows, expected) {
  const applied = new Map(rows.map((row) => [row.id, row.checksum]));
  const problems = [];
  for (const [id, checksum] of expected) {
    const actual = applied.get(id);
    if (actual === undefined) problems.push(`${id}: not applied`);
    else if (actual !== checksum) problems.push(`${id}: checksum ${actual}, Node has ${checksum}`);
  }
  for (const id of applied.keys()) {
    if (!expected.has(id)) problems.push(`${id}: not a known migration`);
  }
  return problems;
}

/** 读会话库的迁移账本（只读打开）。 */
export async function readLedger(file) {
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(file, { readOnly: true });
  try {
    return db
      .prepare("select id, checksum from schema_migration order by id")
      .all()
      .map(({ id, checksum }) => ({ id, checksum }));
  } finally {
    db.close();
  }
}

/** 运行一个进程直到退出：stdin 关闭（`-p` 不等管道里的输入），收集 stdout 与 stderr。 */
export function run(file, args, options) {
  return new Promise((resolvePromise, reject) => {
    const child = spawn(file, args, { ...options, stdio: ["ignore", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.setEncoding("utf8").on("data", (chunk) => (stdout += chunk));
    child.stderr.setEncoding("utf8").on("data", (chunk) => (stderr += chunk));
    child.once("error", reject);
    child.once("close", (code, signal) => resolvePromise({ code, signal, stdout, stderr }));
  });
}

async function smoke(directory, id) {
  const selected = platforms.find((item) => item.id === id);
  if (!selected) throw new Error(`Unknown platform ${id}`);
  const files = (await readdir(directory)).filter((file) => file.endsWith(".tgz"));
  const entry = files.find((file) => file === `mbearo-zcode-${metadata.version}.tgz`);
  const platform = files.find((file) => file === `mbearo-zcode-${id}-${metadata.version}.tgz`);
  assert.ok(entry && platform, "Need the entry and native platform tarballs");
  const temp = await mkdtemp(join(tmpdir(), "zcode-npm-smoke-"));
  try {
    await writeFile(join(temp, "package.json"), '{"private":true}\n');
    await npm(
      [
        "install",
        "--offline",
        "--ignore-scripts",
        "--no-audit",
        "--no-fund",
        join(directory, entry),
        join(directory, platform),
      ],
      { cwd: temp },
    );
    const launcher = join(temp, "node_modules/@mbearo/zcode/bin/zcode.cjs");
    const env = { ...process.env, HOME: temp, USERPROFILE: temp, ZCODE_DATA_BASE_DIR: temp };
    const version = await run(process.execPath, [launcher, "--version"], { cwd: temp, env });
    assert.equal(version.code, 0, version.stderr);
    assert.equal(version.stdout.trim(), `zcode ${metadata.version} (rust-cli)`);
    console.log(`npm install smoke: ${id}: ${version.stdout.trim()}`);

    const args = ["-p", "-r", MISSING_SESSION, "x"];
    const opened = await run(process.execPath, [launcher, ...args], { cwd: temp, env });
    assert.equal(opened.code, 1, `zcode ${args.join(" ")}: ${opened.stderr}`);
    assert.ok(opened.stderr.includes(NO_CONVERSATION), `zcode ${args.join(" ")}: ${opened.stderr}`);
    const rows = await readLedger(join(temp, ...DATABASE));
    const problems = ledgerProblems(rows, expectedLedger(await readFile(LEDGER_TABLE, "utf8")));
    assert.deepEqual(
      problems,
      [],
      `session store migrations differ from Node:\n${problems.join("\n")}`,
    );
    console.log(
      `npm install smoke: ${id}: session store opened, ${rows.length} migration checksums match Node`,
    );

    const installed = join(temp, `node_modules/@mbearo/zcode-${id}`);
    const missing = await installedPlatformProblems(installed, selected.os);
    assert.deepEqual(missing, [], `platform package: ${missing.join(", ")}`);
    const native = join(installed, "bin", executableFile(selected.os));
    if (selected.os === "linux") {
      const dynamic = dynamicCxxRuntime(inspectElf(await readFile(native)).needed);
      assert.deepEqual(dynamic, [], `the C++ runtime must be static: ${dynamic.join(", ")}`);
    }
    for (const tool of embeddedTools(selected.os)) {
      const shown = await run(native, ["--version"], {
        cwd: temp,
        env: { ...env, ARGV0: tool.name },
      });
      assert.equal(shown.code, 0, `ARGV0=${tool.name}: ${shown.stderr}`);
      assert.ok(tool.matches(shown.stdout), `ARGV0=${tool.name}: ${shown.stdout}`);
      console.log(`npm install smoke: ${id}: embedded ${shown.stdout.split("\n")[0]}`);
    }
  } finally {
    await rm(temp, { recursive: true, force: true, maxRetries: 3 });
  }
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  await smoke(resolve(process.argv[2]), process.argv[3]);
}
