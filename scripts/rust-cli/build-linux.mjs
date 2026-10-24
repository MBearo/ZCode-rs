#!/usr/bin/env node
// 在 macOS（或任何有 zig 的主机）上交叉编译 Linux 版 zcode（docs/specs/rust-cli/linux.md）：
//   node scripts/rust-cli/build-linux.mjs [--target <triple>]… [--clippy]
// 只设环境变量、写包装脚本、调 cargo；构建完检查产物（静态链接、架构、没有本机路径）。
import { execFile, spawn } from "node:child_process";
import { access, chmod, mkdir, readFile, stat, writeFile } from "node:fs/promises";
import { constants } from "node:fs";
import { homedir } from "node:os";
import { delimiter, dirname, join, resolve } from "node:path";
import { promisify } from "node:util";

import { BINARY_NAME } from "./binary.mjs";
import { currentVersion } from "./build-number.mjs";
import { checkArtifact, hasAllocator, hostPathHits, inspectElf } from "./linux/elf.mjs";
import {
  TARGETS,
  cargoArgs,
  cargoEnv,
  launcher,
  parseCli,
  remapFlags,
  rustflagsConfig,
} from "./linux/toolchain.mjs";

const execFileAsync = promisify(execFile);
const root = resolve(import.meta.dirname, "../..");
const manifest = join(root, "packages/rust-cli/Cargo.toml");
// cargo、rustup、rustc 在 packages/rust-cli 下执行：rustup 只从当前目录往上找 rust-toolchain.toml，
// target 列表、sysroot 与 commit 都要是那个工具链的（工作线 F46）。
const workspace = dirname(manifest);
const windows = process.platform === "win32";

async function output(command, args) {
  const { stdout } = await execFileAsync(command, args, {
    cwd: workspace,
    maxBuffer: 64 * 1024 * 1024,
  });
  return stdout.trim();
}

async function executable(path) {
  try {
    await access(path, windows ? constants.F_OK : constants.X_OK);
    return true;
  } catch {
    return false;
  }
}

/** zig：环境变量 `ZIG`，没有时在 PATH 里找。 */
async function findZig() {
  if (process.env.ZIG) return process.env.ZIG;
  for (const dir of (process.env.PATH ?? "").split(delimiter).filter(Boolean)) {
    const candidate = join(dir, windows ? "zig.exe" : "zig");
    if (await executable(candidate)) return candidate;
  }
  throw new Error("找不到 zig：请安装 zig 0.15（例如 Homebrew 的 zig），或用环境变量 ZIG 指定路径");
}

async function preflight(targets) {
  for (const name of ["RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS"]) {
    if (process.env[name]) {
      throw new Error(
        `请先清掉 ${name}：它优先于 target.<triple>.rustflags，会让路径改写失效（linux.md §3.2）`,
      );
    }
  }
  const installed = (await output("rustup", ["target", "list", "--installed"])).split(/\s+/);
  const missing = targets.filter((triple) => !installed.includes(triple));
  if (missing.length)
    throw new Error(`没有安装 target：请先运行 rustup target add ${missing.join(" ")}`);
  const zig = await findZig();
  const zigVersion = await output(zig, ["version"]);
  const commit = (await output("rustc", ["-vV"])).match(/^commit-hash: (\S+)$/m)?.[1];
  if (!commit) throw new Error("rustc -vV 没有给出 commit-hash");
  const sysroot = await output("rustc", ["--print", "sysroot"]);
  const metadata = JSON.parse(
    await output("cargo", [
      "metadata",
      "--format-version",
      "1",
      "--no-deps",
      "--manifest-path",
      manifest,
    ]),
  );
  return { zig, zigVersion, commit, sysroot, targetDir: metadata.target_directory };
}

/** 生成 target/zig/<triple>/ 下的 cc、c++ 与 ar 包装脚本，返回它们的路径。 */
async function writeLaunchers({ targetDir, triple, zig }) {
  const dir = join(targetDir, "zig", triple);
  await mkdir(dir, { recursive: true });
  const paths = {};
  for (const tool of ["cc", "c++", "ar"]) {
    const { name, text } = launcher({
      windows,
      node: process.execPath,
      script: join(root, "scripts/rust-cli/linux/zig-tool.mjs"),
      zig,
      tool,
      zigTarget: TARGETS[triple].zig,
    });
    const path = join(dir, name);
    await writeFile(path, text);
    if (!windows) await chmod(path, 0o755);
    paths[tool] = path;
  }
  return paths;
}

function runCargo(args, env) {
  return new Promise((resolveRun, rejectRun) => {
    const child = spawn("cargo", args, { cwd: workspace, env, stdio: "inherit" });
    child.on("error", rejectRun);
    child.on("exit", (code) =>
      code === 0
        ? resolveRun()
        : rejectRun(
            new Error(
              `cargo ${args[0]} --target ${args[args.indexOf("--target") + 1]} 退出码 ${code}`,
            ),
          ),
    );
  });
}

async function fileDescription(path) {
  try {
    return await output("file", ["-b", path]);
  } catch {
    return "（本机没有 file 命令，跳过）";
  }
}

/** 程序自己的字面量里含本机路径前缀的：找 shell 的候选目录（cli `shell.rs`，linux.md §8.1）。 */
const LITERALS = ["/opt/homebrew/bin"];

/** 本机路径的前缀（linux.md §4）：固定的三项加本机的 home、CARGO_HOME、RUSTUP_HOME、仓库根。 */
function needles(cargoHome) {
  const home = homedir();
  const local = [home, cargoHome, process.env.RUSTUP_HOME || join(home, ".rustup"), root];
  return [
    "/Users/",
    "/Volumes/",
    "/opt/homebrew",
    ...local.filter((path) => path && path.length >= 4),
  ];
}

/** 检查一个产物并打印结果，返回不通过的项。 */
async function report({ triple, targetDir, cargoHome, version, debug }) {
  // 产物是 Linux 的 ELF，没有 .exe 后缀（主机是 Windows 时也一样）。
  const path = join(targetDir, triple, debug ? "debug" : "release", BINARY_NAME);
  const bytes = await readFile(path);
  const elf = inspectElf(bytes);
  const hits = hostPathHits(bytes, needles(cargoHome), LITERALS);
  const allocator = hasAllocator(bytes);
  // debug 构建带调试信息（里面有源码的本机路径），只给本机的测试用，不发布：不查本机路径。
  const problems = checkArtifact({
    elf,
    hits: debug ? [] : hits,
    machine: TARGETS[triple].machine,
    allocator,
  });
  const size = (await stat(path)).size;
  const lines = [
    `== ${triple} ==`,
    `产物：${path}`,
    `大小：${size} 字节（${(size / 1024 / 1024).toFixed(1)} MiB）`,
    `版本：${version}`,
    `file：${await fileDescription(path)}`,
    `ELF：${elf.type}，e_machine=${elf.machine}，解释器 ${elf.interpreter ?? "无"}，共享库 ${elf.needed.length ? elf.needed.join("、") : "无"}`,
    `本机路径：${hits.length ? hits.map((hit) => `${hit.needle} × ${hit.count}`).join("，") : "无"}`,
    `分配器：${allocator ? "mimalloc" : "没有 mimalloc"}`,
    problems.length
      ? `检查：不通过\n  - ${problems.join("\n  - ")}`
      : debug
        ? "检查：通过（静态链接、架构正确、mimalloc；debug 构建只给本机测试，不查本机路径）"
        : "检查：通过（静态链接、架构正确、没有本机路径、mimalloc）",
  ];
  process.stdout.write(`${lines.join("\n")}\n`);
  return problems;
}

async function main() {
  const { targets, clippy, debug } = parseCli(process.argv.slice(2));
  const { zig, zigVersion, commit, sysroot, targetDir } = await preflight(targets);
  const cargoHome = process.env.CARGO_HOME || join(homedir(), ".cargo");
  const config = (triple) => rustflagsConfig(triple, remapFlags({ cargoHome, sysroot, commit }));
  process.stdout.write(`zig ${zigVersion}（${zig}）\n`);
  const versions = new Map();
  for (const triple of targets) {
    const { cc, "c++": cxx, ar } = await writeLaunchers({ targetDir, triple, zig });
    const env = {
      ...process.env,
      ...cargoEnv({ triple, cc, cxx, ar, cache: join(targetDir, "zig", "cache") }),
    };
    await runCargo(cargoArgs({ triple, clippy, debug, manifest, config: config(triple) }), env);
    // 计数文件在仓库里（F54，version.md §2）：按顺序构建时每个 triple 各加 1，构建完马上读这一次的号。debug 构建不计数，
    // 版本是 Cargo.toml 的版本。
    if (!clippy) versions.set(triple, debug ? "（debug：Cargo.toml 的版本）" : await currentVersion());
  }
  if (clippy) return 0;
  let failed = 0;
  for (const triple of targets) {
    if ((await report({ triple, targetDir, cargoHome, version: versions.get(triple), debug })).length)
      failed += 1;
  }
  return failed ? 1 : 0;
}

try {
  process.exitCode = await main();
} catch (error) {
  process.stderr.write(`build-linux：${error.message}\n`);
  process.exitCode = 1;
}
