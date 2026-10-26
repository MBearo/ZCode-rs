// node-interop 的 App 协议会话互读（docs/specs/rust-cli/app-protocol.md §10.4 第 3 条；第十段 `checkCompat` 调用）。
//
// Node 的 app-server 写的会话，rust 的 `app-server` 冷订阅；rust 写的会话，Node 冷订阅。两边的快照都过 App 的 zod 与 V4
// 装配器，行（回合、输入、正文、工具调用）与对方自己冷打开时相同。用协议对拍框架的 cold 与 cold-rust 两个场景
// （scripts/rust-cli/app-protocol/parity，只在本仓库）：导出树里没有那个框架，这一段跳过。rust 用 node-interop 构建的
// debug 产物。
import { spawnSync } from "node:child_process";
import { existsSync } from "node:fs";
import { join, resolve } from "node:path";
import { binaryPath } from "./binary.mjs";

const repoRoot = resolve(import.meta.dirname, "../..");

/** 跑两个冷打开的场景。不在本仓库时 `skipped`；否则给 expectEqual 的标签与两边的结论（结果应为 “same”）。 */
export function checkAppInterop() {
  const runner = join(repoRoot, "scripts/rust-cli/app-protocol/parity/run.ts");
  if (!existsSync(runner)) return { skipped: true };
  const result = spawnSync(
    process.execPath,
    ["--import", "tsx", runner, "--a", "node", "--b", "rust", "--only", "cold,cold-rust"].concat([
      "--rust",
      binaryPath(repoRoot),
    ]),
    { cwd: repoRoot, encoding: "utf8", maxBuffer: 16 * 1024 * 1024 },
  );
  const output = `${result.stdout ?? ""}${result.stderr ?? ""}`.trim();
  return {
    skipped: false,
    label: "App protocol: Node's and rust's app-servers cold-open each other's sessions alike",
    output: result.status === 0 ? "same" : output,
    expected: "same",
  };
}
