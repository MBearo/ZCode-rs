#!/usr/bin/env node
// packages/rust-cli 的测试入口（docs/specs/rust-cli/README.md §9）。
import { execFile, spawn } from "node:child_process";
import { join, resolve } from "node:path";
import { promisify } from "node:util";
import { BINARY_NAME } from "./binary.mjs";
import { featureArgs } from "./cargo-features.mjs";
import { requireCargoTools } from "./cargo-tools.mjs";
import { sshTestEnv } from "./ssh-docker.mjs";

const execFileAsync = promisify(execFile);

// 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml（工作线 F46）。
const workspace = resolve(import.meta.dirname, "../../packages/rust-cli");

await requireCargoTools(["nextest"], { cwd: workspace });
// 有 parity 测试的数据时带 `--features parity`（docs/specs/rust-cli/export.md §5）。
const features = await featureArgs(workspace);
// SSH 的 Docker 测试（docs/specs/rust-cli/remote-exec.md §19.12）：有 Docker 就交叉编译 Docker 架构的 debug musl 产物。
const ssh = await sshTestEnv({
  platform: process.platform,
  given: process.env.ZCODE_TEST_SSH_ARTIFACT,
  dockerArch: async () => {
    const { stdout } = await execFileAsync("docker", ["info", "--format", "{{.Architecture}}"]);
    return stdout.trim();
  },
  build: async (triple) => {
    const script = resolve(import.meta.dirname, "build-linux.mjs");
    try {
      await execFileAsync(process.execPath, [script, "--debug", "--target", triple], {
        maxBuffer: 64 * 1024 * 1024,
      });
    } catch (error) {
      const tail = String(error.stderr || error.stdout || error.message).trim().split("\n").at(-1);
      throw new Error(tail);
    }
    return join(workspace, "target", triple, "debug", BINARY_NAME);
  },
});
process.stdout.write(`${ssh.message}\n`);
await new Promise((resolveRun, rejectRun) => {
  const child = spawn("cargo", ["nextest", "run", "--locked", "--workspace", ...features], {
    cwd: workspace,
    stdio: "inherit",
    env: { ...process.env, ...ssh.env },
  });
  child.on("error", rejectRun);
  child.on("exit", (code) =>
    code === 0 ? resolveRun() : rejectRun(new Error(`cargo nextest exited with ${code}`)),
  );
});
