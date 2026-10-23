// scripts/rust-cli/npm/smoke.mjs 的测试：期望的迁移校验和从哪里来、账本怎么比较、库怎么读、进程怎么跑。
// 安装与运行打好的包只在 CI 的 smoke 里做（要本平台的 release 二进制）。
import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, readdir, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import test from "node:test";
import {
  LEDGER_TABLE,
  MISSING_SESSION,
  NO_CONVERSATION,
  dynamicCxxRuntime,
  embeddedTools,
  expectedLedger,
  installedPlatformProblems,
  ledgerProblems,
  readLedger,
  run,
} from "./smoke.mjs";

const NODE_0001 = "60e2d6a38ab36f31417c4f92c02690c96c7dcaaa0b6abe1741117d62a55c6462";
// 0.1.52 的 Windows 版本把 0001 的 SQL 编成 CRLF 后算出的值。
const CRLF_0001 = "128ee35d36193fb3bd6b107c91592ebfcc8e9ca3a95057d443c51befb78ff206";

test("the expected checksums are the Node ledger and match the embedded SQL by the Node rule", async () => {
  const expected = expectedLedger(await readFile(LEDGER_TABLE, "utf8"));
  assert.equal(expected.size, 24);
  assert.equal(expected.get("0001_base_session_store"), NODE_0001);
  const folder = join(dirname(LEDGER_TABLE), "migrations");
  const sql = (await readdir(folder)).filter((file) => file.endsWith(".sql")).sort();
  assert.deepEqual(sql, [...expected.keys()].map((id) => `${id}.sql`).sort());
  for (const [id, checksum] of expected) {
    // Node 的 SQL 在 JS 模板字符串里：CRLF 与单独的 CR 都成了 LF，校验和是 sha256(sql.trim())。
    const text = (await readFile(join(folder, `${id}.sql`), "utf8")).replace(/\r\n?/gu, "\n");
    assert.equal(createHash("sha256").update(text.trim()).digest("hex"), checksum, id);
  }
});

test("a broken or truncated EXPECTED table is an error", () => {
  assert.throws(() => expectedLedger("fn main() {}"), /no EXPECTED table/u);
  const short = `const EXPECTED: [(&str, &str); 2] = [\n    ("0001_a", "${NODE_0001}"),\n];`;
  assert.throws(() => expectedLedger(short), /declares 2 migrations but lists 1/u);
  // Windows 检出的 CRLF 源码也认。
  const crlf = `const EXPECTED: [(&str, &str); 1] = [\r\n    (\r\n        "0001_a",\r\n        "${NODE_0001}",\r\n    ),\r\n];`;
  assert.deepEqual([...expectedLedger(crlf)], [["0001_a", NODE_0001]]);
});

test("the ledger must hold every known migration with the Node checksum and nothing else", () => {
  const expected = new Map([
    ["0001_base_session_store", NODE_0001],
    ["0002_local_setting", "b".repeat(64)],
  ]);
  const good = [
    { id: "0001_base_session_store", checksum: NODE_0001 },
    { id: "0002_local_setting", checksum: "b".repeat(64) },
  ];
  assert.deepEqual(ledgerProblems(good, expected), []);
  assert.deepEqual(ledgerProblems([{ ...good[0], checksum: CRLF_0001 }, good[1]], expected), [
    `0001_base_session_store: checksum ${CRLF_0001}, Node has ${NODE_0001}`,
  ]);
  assert.deepEqual(
    ledgerProblems([good[1], { id: "0099_x", checksum: "c".repeat(64) }], expected),
    ["0001_base_session_store: not applied", "0099_x: not a known migration"],
  );
});

test("the ledger is read from a real SQLite file", async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "rust-cli-smoke-"));
  t.after(() => rm(dir, { recursive: true, force: true, maxRetries: 3 }));
  const file = join(dir, "db.sqlite");
  const { DatabaseSync } = await import("node:sqlite");
  const db = new DatabaseSync(file);
  db.exec("pragma journal_mode = wal");
  db.exec(
    "create table schema_migration (id text primary key, checksum text not null, app_version text, time_applied integer not null)",
  );
  db.prepare("insert into schema_migration values (?, ?, ?, ?)").run(
    "0002_b",
    "b".repeat(64),
    "0.2.0",
    2,
  );
  db.prepare("insert into schema_migration values (?, ?, ?, ?)").run(
    "0001_a",
    NODE_0001,
    "0.2.0",
    1,
  );
  db.close();
  assert.deepEqual(await readLedger(file), [
    { id: "0001_a", checksum: NODE_0001 },
    { id: "0002_b", checksum: "b".repeat(64) },
  ]);
});

test("processes run with stdin closed and report the exit code and output", async () => {
  // stdin 是关着的：读到结尾就退出，不会等管道里的输入。
  const script =
    'process.stdin.resume().on("end", () => { console.log("out"); console.error("eof"); process.exit(3); });';
  const result = await run(process.execPath, ["-e", script], {});
  assert.deepEqual(
    { code: result.code, stdout: result.stdout, stderr: result.stderr },
    { code: 3, stdout: "out\n", stderr: "eof\n" },
  );
  assert.equal(NO_CONVERSATION, `No conversation found with session ID: ${MISSING_SESSION}`);
});

test("the embedded search tools to check per platform", () => {
  // 原生二进制在 ARGV0 是工具名时直接跑内嵌的工具；Windows 没有 bfs。
  assert.deepEqual(
    embeddedTools("darwin").map((tool) => tool.name),
    ["rg", "ugrep", "bfs"],
  );
  assert.deepEqual(
    embeddedTools("linux").map((tool) => tool.name),
    ["rg", "ugrep", "bfs"],
  );
  assert.deepEqual(
    embeddedTools("win32").map((tool) => tool.name),
    ["rg", "ugrep"],
  );
  const [rg, ugrep, bfs] = embeddedTools("darwin");
  assert.ok(rg.matches("ripgrep 15.2.0\n"));
  assert.ok(ugrep.matches("ugrep 7.8.4 aarch64-apple-darwin +neon/AArch64; -P:pcre2jit\n"));
  assert.ok(bfs.matches("bfs 4.1.4\n"));
  // 没有分派时打印的是 CLI 自己的版本。
  assert.ok(!rg.matches("zcode 0.1.55 (rust-cli)\n"));
});

test("the installed platform package has zcode-rs (zcode.exe on Windows) and the third-party notices", async (t) => {
  const { mkdir, writeFile } = await import("node:fs/promises");
  const dir = await mkdtemp(join(tmpdir(), "zcode-smoke-installed-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  assert.deepEqual(await installedPlatformProblems(dir, "linux"), [
    "bin/zcode-rs is missing",
    "THIRD-PARTY-NOTICES.txt is missing",
  ]);
  await mkdir(join(dir, "bin"));
  // Windows 上是 zcode.exe（工作线 F71）：只有 cargo 产物的名字不算。
  await writeFile(join(dir, "bin", "zcode-rs.exe"), "");
  await writeFile(join(dir, "THIRD-PARTY-NOTICES.txt"), "x");
  assert.deepEqual(await installedPlatformProblems(dir, "win32"), ["bin/zcode.exe is missing"]);
  await writeFile(join(dir, "bin/zcode.exe"), "");
  assert.deepEqual(await installedPlatformProblems(dir, "win32"), []);
  // 旧名字的二进制不算。
  assert.deepEqual(await installedPlatformProblems(dir, "darwin"), ["bin/zcode-rs is missing"]);
});

test("the linux-gnu binary links libstdc++ statically", () => {
  // Rust 的 linux-gnu 二进制照常要 glibc 与 libgcc_s；内嵌的 ugrep 的 C++ 运行库要静态链接进去。
  assert.deepEqual(
    dynamicCxxRuntime(["libgcc_s.so.1", "libm.so.6", "libc.so.6", "ld-linux-x86-64.so.2"]),
    [],
  );
  assert.deepEqual(dynamicCxxRuntime(["libstdc++.so.6", "libc.so.6", "libc++.so.1"]), [
    "libstdc++.so.6",
    "libc++.so.1",
  ]);
});
