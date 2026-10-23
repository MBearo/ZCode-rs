#!/usr/bin/env node
// packages/rust-cli 的测试入口（docs/specs/rust-cli/README.md §9）。
import { spawn } from "node:child_process";
import { resolve } from "node:path";
import { featureArgs } from "./cargo-features.mjs";
import { requireCargoTools } from "./cargo-tools.mjs";

// 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml（工作线 F46）。
const workspace = resolve(import.meta.dirname, "../../packages/rust-cli");

await requireCargoTools(["nextest"], { cwd: workspace });
// 有 parity 测试的数据时带 `--features parity`（docs/specs/rust-cli/export.md §5）。
const features = await featureArgs(workspace);
await new Promise((resolveRun, rejectRun) => {
  const child = spawn("cargo", ["nextest", "run", "--locked", "--workspace", ...features], {
    cwd: workspace,
    stdio: "inherit",
  });
  child.on("error", rejectRun);
  child.on("exit", (code) =>
    code === 0 ? resolveRun() : rejectRun(new Error(`cargo nextest exited with ${code}`)),
  );
});
