#!/usr/bin/env node
// `pnpm check:rust-cli` 的 clippy（docs/specs/rust-cli/README.md §9，export.md §5）：全部 target，警告即失败；
// 有 parity 测试的数据时带 `--features parity`（cargo-features.mjs）。
//   node scripts/rust-cli/clippy.mjs                    本机目标
//   node scripts/rust-cli/clippy.mjs --target windows   交叉检查 x86_64-pc-windows-msvc（performance.md §8）
// 交叉检查 Windows 时（主机不是 Windows）：C / C++ 依赖的编译器与归档器换成桩（windows/stub-tool.mjs），只检查
// Rust 代码，包括 `#[cfg(windows)]` 的代码与测试；不链接、不运行。
import { spawn } from "node:child_process";
import { chmod, mkdir, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { featureArgs } from "./cargo-features.mjs";
import { clippyArgs, parseCli, stubEnv, stubLauncher } from "./windows/cross.mjs";

// 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml（工作线 F46）。
const workspace = resolve(import.meta.dirname, "../../packages/rust-cli");

/** 桩工具的包装脚本写在 target/msvc-stub/ 下（`cargo clean` 一起删掉）。 */
async function stubTools() {
  const dir = join(workspace, "target", "msvc-stub");
  await mkdir(dir, { recursive: true });
  const script = join(import.meta.dirname, "windows", "stub-tool.mjs");
  const paths = {};
  for (const tool of ["cc", "ar"]) {
    const path = join(dir, tool);
    await writeFile(path, stubLauncher({ node: process.execPath, script, tool }));
    await chmod(path, 0o755);
    paths[tool] = path;
  }
  return paths;
}

let parsed;
try {
  parsed = parseCli(process.argv.slice(2));
} catch (error) {
  console.error(error.message);
  process.exit(2);
}
const { windows } = parsed;
const env =
  windows && process.platform !== "win32"
    ? { ...process.env, ...stubEnv(await stubTools()) }
    : process.env;
const args = clippyArgs({ windows, features: await featureArgs(workspace) });
const child = spawn("cargo", args, { cwd: workspace, env, stdio: "inherit" });
child.on("error", (error) => {
  console.error(error.message);
  process.exitCode = 1;
});
child.on("exit", (code) => {
  process.exitCode = code ?? 1;
});
