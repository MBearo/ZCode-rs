#!/usr/bin/env node
"use strict";

const { spawn } = require("node:child_process");
const { randomUUID } = require("node:crypto");
const { existsSync, realpathSync } = require("node:fs");
const { mkdtemp, readFile, rm, stat } = require("node:fs/promises");
const { tmpdir } = require("node:os");
const { dirname, join, parse, resolve, win32 } = require("node:path");
const platforms = require("../platforms.json");
const entry = require("../package.json");

// 自动更新的安装方式标记（docs/specs/rust-cli/update.md §2）：二进制只认这些变量判断自己由哪个包管理器安装、
// 入口包与平台包叫什么。先删掉从父进程继承来的，再按本次的判断设一组新的（继承来的标记可能属于另一次安装）。
const MARKERS = {
  npm: "ZCODE_MANAGED_BY_NPM",
  pnpm: "ZCODE_MANAGED_BY_PNPM",
  bun: "ZCODE_MANAGED_BY_BUN",
};
const PACKAGE_VARIABLE = "ZCODE_MANAGED_PACKAGE";
const PLATFORM_PACKAGE_VARIABLE = "ZCODE_MANAGED_PLATFORM_PACKAGE";
// Windows 的更新交接（docs/specs/rust-cli/update.md §8.1）：运行中的 zcode.exe 换不掉，二进制只往这个路径写目标版本、
// 照常退出，启动器在它退出之后自己拼命令执行。只在 Windows 上设；Unix 上 execve，只删掉继承来的。
const HANDOFF_VARIABLE = "ZCODE_MANAGED_UPDATE_HANDOFF";
// 交接文件里唯一认的数据：npm 的 dist-tag `latest` 或 `<数字>.<数字>.<数字>`。其他一律拒绝，不执行任何命令。
const UPDATE_VERSION = /^(?:latest|\d+\.\d+\.\d+)$/u;
// 拼进命令行的包名（cmd.exe 里没有要转义的字符）。
const PACKAGE_NAME = /^(?:@[a-z0-9][a-z0-9._~-]*\/)?[a-z0-9][a-z0-9._~-]*$/u;
// 安装命令（与二进制在 Unix 上用的相同，update.md §2），加 `@<version>`。
const UPDATE_VERBS = { npm: ["install", "-g"], pnpm: ["add", "-g"], bun: ["install", "-g"] };
// Windows 的控制台事件：Ctrl+C、Ctrl+Break、关闭控制台（docs/specs/rust-cli/session-lifecycle.md §3.5.1）。
const WINDOWS_SIGNALS = ["SIGINT", "SIGBREAK", "SIGHUP"];
// 平台包里的可执行文件（docs/specs/rust-cli/cli-args.md §13）：用户敲的命令是本包 `bin` 的 `zcode`，macOS / Linux 起的原生
// 程序叫 `zcode-rs`，进程列表里显示它。package.mjs 打包时用同一个名字（cargo 产物在三个平台上都叫 zcode-rs）。
const EXECUTABLE = "zcode-rs";
// Windows 上就叫命令名（工作线 F71，用户决定 2026-10-23）：任务管理器与 tasklist 只显示 exe 的文件名、改不了显示名，
// 所以不叫 zcode-rs.exe。命令名取本包 `bin` 的唯一一项。
const [COMMAND] = Object.keys(entry.bin);

/** 平台包 `bin/` 下的文件名：Windows 是 `<命令名>.exe`，其余平台是 {@link EXECUTABLE}。 */
function executableFile(os) {
  return os === "win32" ? `${COMMAND}.exe` : EXECUTABLE;
}

function selectPlatform(platform, arch, glibc) {
  const selected = platforms.find((item) => item.os === platform && item.cpu === arch);
  if (!selected || (selected.libc === "glibc" && !glibc)) {
    throw new Error(
      `Unsupported platform: ${platform}/${arch}${platform === "linux" ? " (musl)" : ""}`,
    );
  }
  return selected;
}

/** 这个目录是不是 pnpm 管理、且其中的入口包就是本包。 */
function isPnpmOwnedInstall(nodeModulesDir, packageRoot, name) {
  if (!existsSync(join(nodeModulesDir, ".modules.yaml"))) return false;
  try {
    return realpathSync(join(nodeModulesDir, ...name.split("/"))) === packageRoot;
  } catch {
    return false;
  }
}

/**
 * 安装本包的包管理器（不认 Vite+）：从包的真实位置与入口脚本所在目录往上找 pnpm 的
 * `node_modules/.modules.yaml`；再看 bun 的痕迹；其余（包括 `npx`）按 npm。
 */
function detectPackageManager({ packageRoot, entrypointDir, name, env }) {
  for (const startDir of new Set([packageRoot, entrypointDir])) {
    const root = parse(startDir).root;
    for (let current = startDir; current !== root; current = dirname(current)) {
      if (isPnpmOwnedInstall(join(current, "node_modules"), packageRoot, name)) return "pnpm";
    }
    if (isPnpmOwnedInstall(join(root, "node_modules"), packageRoot, name)) return "pnpm";
  }
  const userAgent = env.npm_config_user_agent || "";
  if (/\bbun\//.test(userAgent)) return "bun";
  if ((env.npm_execpath || "").includes("bun")) return "bun";
  if (
    packageRoot.includes(".bun/install/global") ||
    packageRoot.includes(".bun\\install\\global")
  ) {
    return "bun";
  }
  return "npm";
}

/** 子进程的环境：去掉继承的标记（含交接变量），设本次的标记、入口包与平台包。 */
function managedEnv(base, manager, packageName, platformPackage) {
  const env = { ...base };
  for (const name of [
    ...Object.values(MARKERS),
    PACKAGE_VARIABLE,
    PLATFORM_PACKAGE_VARIABLE,
    HANDOFF_VARIABLE,
  ]) {
    delete env[name];
  }
  env[MARKERS[manager]] = "1";
  env[PACKAGE_VARIABLE] = packageName;
  env[PLATFORM_PACKAGE_VARIABLE] = platformPackage;
  return env;
}

/**
 * 起原生二进制：Unix 直接替换进程，TTY、信号与退出码保持原生语义，避免 Ctrl-C 被包装器重复转发；Windows 起子进程，
 * 两条路径都带上同一份环境（标记）。返回 Windows 的子进程（Unix 不返回）。
 */
function launch({ platform, binary, args, env, execve, spawnChild }) {
  if (platform !== "win32") {
    execve(binary, [binary, ...args], env);
    return null;
  }
  return spawnChild(binary, args, { stdio: "inherit", windowsHide: false, env });
}

/** Windows 的交接文件路径：每次启动一个新的，不创建（不更新时什么都不留下）。 */
function handoffPath(temp, id) {
  return join(temp, `zcode-update-${id}.json`);
}

/** 读交接文件并删掉；没有时 null。 */
async function takeHandoff(path, io) {
  let text;
  try {
    text = await io.readFile(path, "utf8");
  } catch (error) {
    if (error.code === "ENOENT") return null;
    throw error;
  }
  await io.rm(path, { force: true });
  return text;
}

/** 交接文件里的目标版本：一行 JSON 对象的 `version`，只认 {@link UPDATE_VERSION}；其余抛错。 */
function handoffVersion(text) {
  let data = null;
  try {
    data = JSON.parse(text);
  } catch {
    // 下面按坏文件处理。
  }
  const version = data !== null && typeof data === "object" ? data.version : undefined;
  if (typeof version !== "string" || !UPDATE_VERSION.test(version)) {
    throw new Error("invalid update handoff");
  }
  return version;
}

/** 安装命令（程序与参数）：启动器自己判定的包管理器、自己的包名、校验过的版本。 */
function updateCommand(manager, packageName, version) {
  if (!UPDATE_VERBS[manager] || !PACKAGE_NAME.test(packageName) || !UPDATE_VERSION.test(version)) {
    throw new Error("invalid update handoff");
  }
  return [manager, ...UPDATE_VERBS[manager], `${packageName}@${version}`];
}

/** 环境里的变量，键名不分大小写（Windows）。 */
function lookup(env, name) {
  const key = Object.keys(env).find((item) => item.toUpperCase() === name);
  return key === undefined ? "" : (env[key] ?? "");
}

/**
 * Windows 上找更新命令（以前在二进制里）：只在 PATH 的绝对路径项（盘符或 UNC）里找，
 * 按 PATHEXT 依次试扩展名，取第一个存在的普通文件；不让项目里的同名命令冒充。
 */
async function findWindowsCommand(name, env, isFile) {
  const extensions = (lookup(env, "PATHEXT") || ".COM;.EXE;.BAT;.CMD").split(";").filter(Boolean);
  for (const entry of lookup(env, "PATH").split(";")) {
    const dir = entry.replaceAll('"', "");
    if (!/^(?:[A-Za-z]:[\\/]|[\\/]{2})/u.test(dir)) continue;
    for (const extension of extensions) {
      const candidate = win32.join(dir, `${name}${extension}`);
      if (await isFile(candidate)) return candidate;
    }
  }
  return null;
}

/** 等子进程结束：`{ code }`（被结束时 code 为 null）或 `{ error }`（起不来）。 */
function finished(child) {
  return new Promise((resolveFinished) => {
    child.once("error", (error) => resolveFinished({ error }));
    child.once("exit", (code) => resolveFinished({ code }));
  });
}

/**
 * 执行更新（update.md §8.1 第 4–8 步）：打印与 Unix 的二进制相同的文字，退出码用包管理器的。`.cmd` / `.bat` 经
 * cmd.exe（整条命令行是一个字符串：程序路径加引号，参数只有校验过的包名与版本），`.exe` / `.com` 直接起；工作目录是
 * 新建的临时目录，用完删掉。
 */
async function runUpdate({ manager, packageName, version, env, io }) {
  const [program, ...args] = updateCommand(manager, packageName, version);
  const line = [program, ...args].join(" ");
  io.stdout.write(`\nUpdating ZCode via \`${line}\`...\n`);
  const found = await findWindowsCommand(program, env, io.isFile);
  if (!found) {
    io.stderr.write(
      `Error: Could not find an absolute update command \`${program}\` on PATH. Please update manually: https://www.npmjs.com/package/${packageName}\n`,
    );
    return 1;
  }
  const cwd = await io.mkdtemp(join(io.tmpdir(), "zcode-update-"));
  try {
    const options = { stdio: "inherit", cwd, env, windowsHide: false };
    const child = /\.(?:cmd|bat)$/iu.test(found)
      ? io.spawn(`"${found}" ${args.join(" ")}`, { ...options, shell: true })
      : io.spawn(found, args, options);
    const result = await finished(child);
    if (result.error) {
      io.stderr.write(`Error: ${result.error.message}\n`);
      return 1;
    }
    if (result.code === 0) {
      io.stdout.write("\n🎉 Update ran successfully! Please restart ZCode.\n");
      return 0;
    }
    const code = result.code ?? 1;
    io.stderr.write(`Error: \`${line}\` failed with status exit code: ${code}\n`);
    return code;
  } finally {
    await io.rm(cwd, { recursive: true, force: true });
  }
}

/**
 * Windows：起子进程并等它退出，之后有交接文件时执行更新；返回启动器的退出码。
 *
 * 缺陷修复（session-lifecycle.md §3.5.1）：以前把 SIGINT、SIGTERM 转成 `child.kill`，Windows 上那是 TerminateProcess，
 * 原生程序的收尾被截断、退出码变 1；SIGBREAK、SIGHUP 没有处理，Node 按缺省先退出，cmd 与还在收尾的原生程序抢控制台。
 * 控制台事件本来就由系统送到同一控制台上的所有进程（包括子进程），所以这里只装空处理器（不让 Node 先结束自己），不转发，
 * 等子进程退出、转发它的退出码。处理器一直留到更新也结束（Ctrl+C 同样由控制台送到包管理器）。
 *
 * 缺陷修复（update.md §8.1，以前的 D7）：以前二进制自己起 npm，npm 换不掉正在运行的 exe（EPERM / EBUSY）。
 */
async function runWindows({ binary, args, env, manager, packageName, io }) {
  const handoff = handoffPath(io.tmpdir(), io.randomUUID());
  const ignore = () => {};
  for (const signal of WINDOWS_SIGNALS) io.signals.on(signal, ignore);
  try {
    const child = launch({
      platform: "win32",
      binary,
      args,
      env: { ...env, [HANDOFF_VARIABLE]: handoff },
      execve: null,
      spawnChild: io.spawn,
    });
    const result = await finished(child);
    if (result.error) {
      io.stderr.write(`zcode: ${result.error.message}\n`);
      await io.rm(handoff, { force: true });
      return 1;
    }
    const code = result.code ?? 1;
    const text = await takeHandoff(handoff, io);
    if (text === null || code !== 0) return code;
    const version = handoffVersion(text);
    return await runUpdate({ manager, packageName, version, env, io });
  } finally {
    for (const signal of WINDOWS_SIGNALS) io.signals.removeListener(signal, ignore);
  }
}

/** 真实的进程、文件与输出（测试换成注入的）。 */
function systemIo() {
  return {
    spawn,
    signals: process,
    tmpdir,
    randomUUID,
    readFile,
    rm,
    mkdtemp,
    isFile: async (path) => {
      try {
        return (await stat(path)).isFile();
      } catch {
        return false;
      }
    },
    stdout: process.stdout,
    stderr: process.stderr,
  };
}

async function main() {
  const selected = selectPlatform(
    process.platform,
    process.arch,
    process.platform === "linux"
      ? process.report.getReport().header.glibcVersionRuntime
      : undefined,
  );
  const name = `@mbearo/zcode-${selected.id}`;
  let manifest;
  try {
    manifest = require.resolve(`${name}/package.json`);
  } catch {
    throw new Error(
      `Missing ${name}@${entry.version}. Reinstall @mbearo/zcode with optional dependencies enabled (npm install -g @mbearo/zcode --include=optional).`,
    );
  }
  const installed = JSON.parse(await readFile(manifest, "utf8"));
  if (installed.version !== entry.version) {
    throw new Error(
      `${name} version ${installed.version} does not match @mbearo/zcode ${entry.version}`,
    );
  }
  const binary = join(dirname(manifest), "bin", executableFile(selected.os));
  const args = process.argv.slice(2);
  const packageRoot = realpathSync(join(__dirname, ".."));
  const manager = detectPackageManager({
    packageRoot,
    entrypointDir: dirname(resolve(process.argv[1])),
    name: entry.name,
    env: process.env,
  });
  const env = managedEnv(process.env, manager, entry.name, name);
  // Unix：execve 换成 zcode-rs，没有中间进程，不转发信号（session-lifecycle.md §3.5.1：终端的 SIGINT 本来就发给整个
  // 前台进程组，spawn 再 `child.kill` 转发会让子进程收到两次）；更新在 zcode-rs 里执行。
  if (process.platform !== "win32") {
    launch({
      platform: process.platform,
      binary,
      args,
      env,
      execve: process.execve,
      spawnChild: spawn,
    });
    return;
  }
  process.exitCode = await runWindows({
    binary,
    args,
    env,
    manager,
    packageName: entry.name,
    io: systemIo(),
  });
}

module.exports = {
  HANDOFF_VARIABLE,
  executableFile,
  selectPlatform,
  detectPackageManager,
  managedEnv,
  launch,
  handoffPath,
  handoffVersion,
  updateCommand,
  findWindowsCommand,
  runWindows,
  main,
};
if (require.main === module) {
  main().catch((error) => {
    console.error(`zcode: ${error.message}`);
    process.exitCode = 1;
  });
}
