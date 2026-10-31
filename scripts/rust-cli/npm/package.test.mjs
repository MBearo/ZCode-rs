import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { EventEmitter } from "node:events";
import { createRequire } from "node:module";
import { chmod, cp, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { promisify } from "node:util";
import test from "node:test";
import { currentVersion } from "../build-number.mjs";
import {
  metadata,
  NOTICES,
  pack,
  platforms,
  stagePackage,
  template,
  validateBinary,
  verifyVersion,
} from "./package.mjs";

const exec = promisify(execFile);
const require = createRequire(import.meta.url);
const {
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
} = require("./package/bin/zcode.cjs");
const { realpath, symlink } = await import("node:fs/promises");
const sourceCommit = "a".repeat(40);
// 入口包名从 package/package.json 取：binary.test.mjs 把字符串里以 `/zcode` 结尾的写法当成写死的可执行文件名。
const ENTRY = metadata.name;
async function fixture(t) {
  const path = await mkdtemp(join(tmpdir(), "rust-cli-npm-"));
  t.after(() => rm(path, { recursive: true, force: true }));
  return path;
}
// 第三方许可证声明的替身：仓库里的那份由 packages/rust-cli 的 vendor 生成，随代码同步过来。
async function notices(temp) {
  const path = join(temp, "THIRD-PARTY-NOTICES.txt");
  await writeFile(path, "ripgrep\r\nMIT\n");
  return path;
}
// 入口包的目录：模板加上打包时写入的版本（模板本身不写版本，版本来自 packages/rust-cli/build-number）。
async function copyEntry(entry) {
  await cp(template, entry, { recursive: true });
  await writeFile(join(entry, "package.json"), JSON.stringify(metadata, null, 2) + "\n");
}
function header(platform) {
  const bytes = Buffer.alloc(256);
  if (platform.os === "darwin") {
    bytes.writeUInt32LE(0xfeedfacf);
    bytes.writeUInt32LE(platform.cpu === "arm64" ? 0x100000c : 0x1000007, 4);
  } else if (platform.os === "linux") {
    bytes.set([127, 69, 76, 70, 2, 1]);
    bytes.writeUInt16LE(platform.cpu === "arm64" ? 183 : 62, 18);
  } else {
    bytes.writeUInt16LE(0x5a4d);
    bytes.writeUInt32LE(128, 0x3c);
    bytes.writeUInt32LE(0x4550, 128);
    bytes.writeUInt16LE(platform.cpu === "arm64" ? 0xaa64 : 0x8664, 132);
  }
  return bytes;
}

test("the npm version is the Cargo major.minor plus build-number", async (t) => {
  // 与 release 编译校验 ZCODE_RELEASE_VERSION 用的是同一个号；模板不写版本。
  assert.equal(await verifyVersion(), metadata.version);
  assert.equal(metadata.version, await currentVersion());
  assert.match(metadata.version, /^\d+\.\d+\.\d+$/u);
  const declared = JSON.parse(await readFile(join(template, "package.json"), "utf8"));
  assert.equal(declared.version, undefined);
  assert.deepEqual(Object.keys(metadata).slice(0, 2), ["name", "version"]);
  const temp = await fixture(t);
  const manifest = join(temp, "Cargo.toml");
  const counter = join(temp, "build-number");
  await writeFile(
    manifest,
    '[workspace]\nmembers = []\n\n[workspace.package]\nversion = "0.1.48"\n',
  );
  await writeFile(counter, "52\n");
  assert.equal(await verifyVersion({ manifest, counter }), "0.1.52");
  await writeFile(counter, "<<<<<<< HEAD\n52\n=======\n53\n>>>>>>> lane\n");
  await assert.rejects(verifyVersion({ manifest, counter }), /is not a build number/u);
  await assert.rejects(verifyVersion({ manifest, counter: join(temp, "missing") }), /is missing/u);
});

test("every supported platform is detected and validated", async () => {
  for (const platform of platforms) {
    assert.equal(selectPlatform(platform.os, platform.cpu, "2.35").id, platform.id);
    validateBinary(header(platform), platform);
    for (const wrong of platforms.filter((item) => item.id !== platform.id)) {
      assert.throws(() => validateBinary(header(platform), wrong), /does not match/u);
    }
  }
  assert.throws(() => selectPlatform("linux", "x64", undefined), /Unsupported/u);
  assert.throws(() => selectPlatform("freebsd", "x64"), /Unsupported/u);
  assert.throws(
    () => validateBinary(Buffer.from("not an executable"), platforms[0]),
    /does not match/u,
  );
});

// Linux 两个架构都发 glibc 的平台包；远端执行也按远端的架构下载这两个包里的执行器。
test("Linux ships glibc packages for x64 and arm64", () => {
  const linux = platforms.filter((item) => item.os === "linux");
  assert.deepEqual(
    linux.map(({ id, cpu, libc, target }) => ({ id, cpu, libc, target })),
    [
      { id: "linux-x64-gnu", cpu: "x64", libc: "glibc", target: "x86_64-unknown-linux-gnu" },
      { id: "linux-arm64-gnu", cpu: "arm64", libc: "glibc", target: "aarch64-unknown-linux-gnu" },
    ],
  );
  assert.equal(selectPlatform("linux", "arm64", "2.35").id, "linux-arm64-gnu");
  assert.throws(() => selectPlatform("linux", "arm64", undefined), /Unsupported/u);
});

// Windows 两个架构都发平台包（docs/specs/rust-cli/embedded-search.md §9.3，工作线 F111）：ARM64 在 GitHub 的 Windows 11 ARM64
// runner 上原生编，冒烟在同一台机器上运行装好的包。
test("Windows ships packages for x64 and arm64", () => {
  const windows = platforms.filter((item) => item.os === "win32");
  assert.deepEqual(
    windows.map(({ id, cpu, target, runner }) => ({ id, cpu, target, runner })),
    [
      { id: "win32-x64", cpu: "x64", target: "x86_64-pc-windows-msvc", runner: "windows-2022" },
      {
        id: "win32-arm64",
        cpu: "arm64",
        target: "aarch64-pc-windows-msvc",
        runner: "windows-11-arm",
      },
    ],
  );
  assert.equal(selectPlatform("win32", "arm64").id, "win32-arm64");
  assert.equal(selectPlatform("win32", "x64").id, "win32-x64");
});

// Windows 上叫 zcode.exe（docs/specs/rust-cli/cli-args.md §13，工作线 F71）：任务管理器与 tasklist 只显示 exe 的文件名，
// 进程名就叫命令名；macOS / Linux 仍是 zcode-rs。
test("the command is zcode and the native executable is zcode-rs, or zcode.exe on Windows", async () => {
  const declared = JSON.parse(await readFile(join(template, "package.json"), "utf8"));
  assert.deepEqual(declared.bin, { zcode: "bin/zcode.cjs" });
  assert.equal(executableFile("darwin"), "zcode-rs");
  assert.equal(executableFile("linux"), "zcode-rs");
  assert.equal(`bin/${executableFile("win32")}`, "bin/zcode.exe");
  assert.match(NOTICES, /packages[\\/]rust-cli[\\/]vendor[\\/]THIRD-PARTY-NOTICES\.txt$/u);
});

test("platform packages contain only binary, provenance, license, notices and metadata", async (t) => {
  const temp = await fixture(t);
  const binary = join(temp, "input");
  await writeFile(binary, header(platforms[0]));
  const thirdParty = await notices(temp);
  const artifact = await pack({
    id: platforms[0].id,
    binary,
    destination: join(temp, "packed"),
    sourceCommit,
    notices: thirdParty,
  });
  assert.deepEqual(artifact.files.map((file) => file.path).sort(), [
    "LICENSE",
    "README.md",
    "THIRD-PARTY-NOTICES.txt",
    "bin/zcode-rs",
    "build.json",
    "package.json",
  ]);
  const folder = join(temp, "packed", platforms[0].id);
  const built = JSON.parse(await readFile(join(folder, "build.json"), "utf8"));
  assert.equal(built.sourceCommit, sourceCommit);
  assert.equal(built.sha256.length, 64);
  const pkg = JSON.parse(await readFile(join(folder, "package.json"), "utf8"));
  assert.deepEqual(pkg.os, ["darwin"]);
  assert.deepEqual(pkg.cpu, ["arm64"]);
  assert.ok(pkg.files.includes("THIRD-PARTY-NOTICES.txt"));
  // 原样复制（许可证原文里有 CRLF）。
  assert.deepEqual(
    await readFile(join(folder, "THIRD-PARTY-NOTICES.txt")),
    await readFile(thirdParty),
  );
  await assert.rejects(
    stagePackage({
      id: platforms[0].id,
      binary: join(temp, "missing"),
      output: join(temp, "bad"),
      sourceCommit,
      notices: thirdParty,
    }),
    /ENOENT/u,
  );
  await assert.rejects(
    stagePackage({
      id: platforms[0].id,
      binary,
      output: folder,
      sourceCommit,
      notices: thirdParty,
    }),
    /EEXIST/u,
  );
  // 没有许可证声明时不打包。
  await assert.rejects(
    stagePackage({
      id: platforms[0].id,
      binary,
      output: join(temp, "no-notices"),
      sourceCommit,
      notices: join(temp, "missing-notices.txt"),
    }),
    /THIRD-PARTY-NOTICES\.txt/u,
  );
});

test("the Windows platform package ships zcode.exe", async (t) => {
  const temp = await fixture(t);
  const windows = platforms.find((item) => item.os === "win32");
  const binary = join(temp, "input.exe");
  await writeFile(binary, header(windows));
  const artifact = await pack({
    id: windows.id,
    binary,
    destination: join(temp, "packed"),
    sourceCommit,
    notices: await notices(temp),
  });
  const files = artifact.files.map((file) => file.path);
  // cargo 产物叫 zcode-rs.exe，打包时复制成 bin/zcode.exe；包里没有别的名字的可执行文件。
  assert.ok(files.includes("bin/zcode.exe"), files.join(", "));
  assert.ok(files.includes("THIRD-PARTY-NOTICES.txt"), files.join(", "));
  assert.deepEqual(
    files.filter((file) => file.startsWith("bin/")),
    ["bin/zcode.exe"],
  );
});

test("entry package pins all optional platform versions and has no install scripts", async (t) => {
  const temp = await fixture(t);
  const artifact = await pack({ id: "entry", destination: join(temp, "packed"), sourceCommit });
  const pkg = JSON.parse(await readFile(join(temp, "packed/entry/package.json"), "utf8"));
  assert.equal(Object.keys(pkg.optionalDependencies).length, platforms.length);
  assert.ok(Object.values(pkg.optionalDependencies).every((version) => version === pkg.version));
  assert.equal(pkg.scripts, undefined);
  assert.ok(artifact.files.some((file) => file.path === "bin/zcode.cjs" && file.mode & 0o111));
  assert.ok(artifact.files.every((file) => !file.path.endsWith(".test.mjs")));
  // 入口包不带二进制，也不带第三方许可证声明。
  assert.ok(artifact.files.every((file) => file.path !== "THIRD-PARTY-NOTICES.txt"));
  assert.deepEqual(pkg.bin, { zcode: "bin/zcode.cjs" });
});

test("launcher explains a missing platform dependency", async (t) => {
  const temp = await fixture(t);
  const entry = join(temp, "entry");
  await copyEntry(entry);
  await assert.rejects(exec(process.execPath, [join(entry, "bin/zcode.cjs")]), (error) => {
    assert.match(error.stderr, /optional dependencies enabled/u);
    return error.code === 1;
  });
});

test(
  "Unix launcher preserves arguments and exit status without an extra signal-forwarding process",
  { skip: process.platform === "win32" },
  async (t) => {
    const temp = await fixture(t);
    const entry = join(temp, "node_modules", ENTRY);
    await copyEntry(entry);
    const platform = selectPlatform(process.platform, process.arch, "2.35");
    const native = join(temp, `node_modules/@mbearo/zcode-${platform.id}`);
    await mkdir(join(native, "bin"), { recursive: true });
    await writeFile(join(native, "package.json"), JSON.stringify({ version: metadata.version }));
    // 启动器 execve 平台包里的 bin/zcode-rs（命令名仍是 zcode）。
    const executable = join(native, "bin/zcode-rs");
    await writeFile(
      executable,
      `#!${process.execPath}\nconst m = Object.fromEntries(Object.entries(process.env).filter(([k]) => k.startsWith("ZCODE_MANAGED_")));\nconsole.log(JSON.stringify({ program: require("node:path").basename(process.argv[1]), argv: process.argv.slice(2), markers: m }));process.exit(23);\n`,
    );
    await chmod(executable, 0o755);
    // 继承来的标记先删掉，再按本次的判断设（docs/specs/rust-cli/update.md §2）：这里不是 pnpm、也没有 bun 的痕迹，按 npm。
    const inherited = {
      ...process.env,
      npm_config_user_agent: "npm/10.9.0 node/v22.16.0",
      npm_execpath: "",
      ZCODE_MANAGED_BY_BUN: "1",
      ZCODE_MANAGED_PACKAGE: "stale",
    };
    await assert.rejects(
      exec(process.execPath, [join(entry, "bin/zcode.cjs"), "a b", "--flag"], { env: inherited }),
      (error) => {
        assert.equal(error.code, 23);
        const seen = JSON.parse(error.stdout);
        assert.equal(seen.program, "zcode-rs");
        assert.deepEqual(seen.argv, ["a b", "--flag"]);
        assert.deepEqual(seen.markers, {
          ZCODE_MANAGED_BY_NPM: "1",
          ZCODE_MANAGED_PACKAGE: ENTRY,
          ZCODE_MANAGED_PLATFORM_PACKAGE: `@mbearo/zcode-${platform.id}`,
        });
        return true;
      },
    );
    await writeFile(join(native, "package.json"), '{"version":"9.9.9"}');
    await assert.rejects(exec(process.execPath, [join(entry, "bin/zcode.cjs")]), (error) => {
      assert.match(error.stderr, /does not match/u);
      return error.code === 1;
    });
  },
);

test("the launcher replaces inherited update markers with its own", () => {
  const env = managedEnv(
    {
      PATH: "/usr/bin",
      ZCODE_MANAGED_BY_NPM: "1",
      ZCODE_MANAGED_BY_BUN: "1",
      ZCODE_MANAGED_PACKAGE: "other",
      ZCODE_MANAGED_PLATFORM_PACKAGE: "other-x",
      // 嵌套启动时从父进程继承来的交接路径也删掉（docs/specs/rust-cli/update.md §2）。
      ZCODE_MANAGED_UPDATE_HANDOFF: "C:\\Temp\\zcode-update-old.json",
    },
    "pnpm",
    ENTRY,
    "@mbearo/zcode-darwin-arm64",
  );
  assert.deepEqual(env, {
    PATH: "/usr/bin",
    ZCODE_MANAGED_BY_PNPM: "1",
    ZCODE_MANAGED_PACKAGE: ENTRY,
    ZCODE_MANAGED_PLATFORM_PACKAGE: "@mbearo/zcode-darwin-arm64",
  });
});

test("Unix execve and Windows spawn both carry the update markers", () => {
  const env = managedEnv({ PATH: "/usr/bin" }, "npm", ENTRY, "@mbearo/zcode-win32-x64");
  const calls = [];
  const execve = (...args) => calls.push(["execve", ...args]);
  const spawnChild = (...args) => {
    calls.push(["spawn", ...args]);
    return { child: true };
  };
  assert.equal(
    launch({
      platform: "darwin",
      binary: "/b/zcode-rs",
      args: ["-p", "x"],
      env,
      execve,
      spawnChild,
    }),
    null,
  );
  assert.deepEqual(
    launch({
      platform: "win32",
      binary: "C:\\b\\zcode.exe",
      args: ["-p"],
      env,
      execve,
      spawnChild,
    }),
    { child: true },
  );
  assert.deepEqual(calls, [
    ["execve", "/b/zcode-rs", ["/b/zcode-rs", "-p", "x"], env],
    ["spawn", "C:\\b\\zcode.exe", ["-p"], { stdio: "inherit", windowsHide: false, env }],
  ]);
  assert.equal(calls[1][3].env.ZCODE_MANAGED_BY_NPM, "1");
});

test("the package manager is detected from pnpm and bun traces, npm otherwise", async (t) => {
  const temp = await fixture(t);
  const name = ENTRY;
  // npm 全局安装：没有 pnpm 的 .modules.yaml，也没有 bun 的痕迹；`npx` 同样按 npm（不区分两者）。
  const npmRoot = join(temp, "npm/lib/node_modules", ENTRY);
  await mkdir(npmRoot, { recursive: true });
  const base = { packageRoot: await realpath(npmRoot), entrypointDir: join(npmRoot, "bin"), name };
  assert.equal(detectPackageManager({ ...base, env: { npm_config_user_agent: "npm/10" } }), "npm");
  assert.equal(detectPackageManager({ ...base, env: {} }), "npm");
  assert.equal(
    detectPackageManager({ ...base, env: { npm_config_user_agent: "bun/1.2.0" } }),
    "bun",
  );
  assert.equal(detectPackageManager({ ...base, env: { npm_execpath: "/x/.bun/bin/bun" } }), "bun");
  // pnpm 全局安装：node_modules/.modules.yaml，入口包是指向 .pnpm 里真实目录的链接。
  const store = join(
    temp,
    `pnpm/global/5/node_modules/.pnpm/@mbearo+zcode@0.1.46/node_modules/${ENTRY}`,
  );
  await mkdir(store, { recursive: true });
  const modules = join(temp, "pnpm/global/5/node_modules");
  await writeFile(join(modules, ".modules.yaml"), "layoutVersion: 5\n");
  await mkdir(join(modules, "@mbearo"), { recursive: true });
  await symlink(store, join(modules, ENTRY), "dir");
  const pnpm = {
    packageRoot: await realpath(store),
    entrypointDir: join(modules, ENTRY, "bin"),
    name,
    env: { npm_config_user_agent: "npm/10" },
  };
  assert.equal(detectPackageManager(pnpm), "pnpm");
  // .modules.yaml 里的同名包不是本包（另一份安装）：不算 pnpm。
  assert.equal(detectPackageManager({ ...pnpm, packageRoot: base.packageRoot }), "npm");
});

// —— Windows 的交接与信号（docs/specs/rust-cli/update.md §8.1、session-lifecycle.md §3.5.1）：注入的 spawn 与文件操作，
// 不起真的进程、不跑 npm，三平台都能跑。

const NPM_CMD = "C:\\nodejs\\npm.cmd";
// 子进程的环境：PATH 里有相对路径项（项目里的同名命令）与绝对路径项；键名是 Windows 的 `Path`。
const WINDOWS_ENV = {
  Path: "node_modules\\.bin;C:\\nodejs",
  PATHEXT: ".EXE;.CMD",
  ZCODE_MANAGED_BY_NPM: "1",
  ZCODE_MANAGED_PACKAGE: ENTRY,
};

/**
 * 注入的进程与文件：第一个 spawn 是原生程序（Windows 的 zcode.exe；`child(options, files, signals)` 决定它做什么，返回它的退出码），之后的是
 * 包管理器（以 `update` 结束：`{ code }` 或 `{ error }`）。文件在内存里。
 */
function fakeIo({ child = () => 0, update = { code: 0 }, commands = [NPM_CMD] } = {}) {
  const files = new Map();
  const calls = [];
  const out = { stdout: "", stderr: "" };
  const signals = new EventEmitter();
  let spawned = 0;
  const io = {
    signals,
    tmpdir: () => "/t",
    randomUUID: () => "u1",
    readFile: async (path) => {
      if (!files.has(path)) throw Object.assign(new Error(`ENOENT: ${path}`), { code: "ENOENT" });
      return files.get(path);
    },
    rm: async (path) => {
      calls.push(["rm", path]);
      files.delete(path);
    },
    mkdtemp: async (prefix) => `${prefix}abc`,
    // Windows 的文件名不分大小写；相对路径项里也有一个 npm.cmd（不该被用）。
    isFile: async (path) =>
      [...commands, "node_modules\\.bin\\npm.cmd"].some(
        (item) => item.toLowerCase() === path.toLowerCase(),
      ),
    spawn: (...args) => {
      calls.push(["spawn", ...args]);
      const process = new EventEmitter();
      process.kill = (signal) => calls.push(["kill", signal]);
      const first = spawned === 0;
      spawned += 1;
      setImmediate(() => {
        if (first) {
          process.emit("exit", child(args.at(-1), files, signals));
        } else if (update.error) {
          process.emit("error", update.error);
        } else {
          process.emit("exit", update.code);
        }
      });
      return process;
    },
    stdout: { write: (text) => (out.stdout += text) },
    stderr: { write: (text) => (out.stderr += text) },
  };
  return { io, files, calls, out, signals };
}

/** 原生程序照 update.md §8.1 写交接文件、以 0 退出。 */
const handsOff = (version) => (options, files) => {
  files.set(options.env[HANDOFF_VARIABLE], `${JSON.stringify({ version })}\n`);
  return 0;
};

function windows(io) {
  return runWindows({
    binary: "C:\\b\\zcode.exe",
    args: ["update"],
    env: WINDOWS_ENV,
    manager: "npm",
    packageName: ENTRY,
    io,
  });
}

test("Windows runs the update after zcode.exe exits, with the command the launcher builds", async () => {
  const fake = fakeIo({ child: handsOff("0.1.55") });
  assert.equal(await windows(fake.io), 0);
  const handoff = handoffPath("/t", "u1");
  const [first, second] = fake.calls.filter((call) => call[0] === "spawn");
  // zcode.exe：环境多一个交接路径，其余与启动器给的相同。
  assert.deepEqual(first, [
    "spawn",
    "C:\\b\\zcode.exe",
    ["update"],
    {
      stdio: "inherit",
      windowsHide: false,
      env: { ...WINDOWS_ENV, [HANDOFF_VARIABLE]: handoff },
    },
  ]);
  // npm：PATH 的绝对路径项里找到的 npm.cmd（相对路径项里的不用），经 cmd.exe、路径加引号，命令带校验过的版本；
  // 工作目录是新建的临时目录，环境没有交接变量。
  assert.deepEqual(second, [
    "spawn",
    // 扩展名的写法取 PATHEXT 的（文件名不分大小写）。
    '"C:\\nodejs\\npm.CMD" install -g @mbearo/zcode@0.1.55',
    {
      stdio: "inherit",
      cwd: join("/t", "zcode-update-abc"),
      env: WINDOWS_ENV,
      windowsHide: false,
      shell: true,
    },
  ]);
  assert.equal(
    fake.out.stdout,
    "\nUpdating ZCode via `npm install -g @mbearo/zcode@0.1.55`...\n\n🎉 Update ran successfully! Please restart ZCode.\n",
  );
  assert.equal(fake.out.stderr, "");
  // 交接文件读后删掉，临时工作目录用完删掉。
  assert.equal(fake.files.size, 0);
  assert.deepEqual(
    fake.calls.filter((call) => call[0] === "rm").map((call) => call[1]),
    [handoff, join("/t", "zcode-update-abc")],
  );
});

test("the exit code is the package manager's, and a missing command is an error", async () => {
  const failed = fakeIo({ child: handsOff("latest"), update: { code: 5 } });
  assert.equal(await windows(failed.io), 5);
  assert.equal(
    failed.out.stdout,
    "\nUpdating ZCode via `npm install -g @mbearo/zcode@latest`...\n",
  );
  assert.equal(
    failed.out.stderr,
    "Error: `npm install -g @mbearo/zcode@latest` failed with status exit code: 5\n",
  );
  // 只有相对路径项里有 npm：找不到，退出码 1，不起包管理器。
  const missing = fakeIo({ child: handsOff("0.1.55"), commands: [] });
  assert.equal(await windows(missing.io), 1);
  assert.equal(
    missing.out.stderr,
    "Error: Could not find an absolute update command `npm` on PATH. Please update manually: https://www.npmjs.com/package/@mbearo/zcode\n",
  );
  assert.equal(missing.calls.filter((call) => call[0] === "spawn").length, 1);
  // 起不来：系统错误，退出码 1。
  const broken = fakeIo({ child: handsOff("0.1.55"), update: { error: new Error("spawn EPERM") } });
  assert.equal(await windows(broken.io), 1);
  assert.equal(broken.out.stderr, "Error: spawn EPERM\n");
});

test("without a handoff, or when zcode.exe fails, nothing is updated", async () => {
  const plain = fakeIo({ child: () => 3 });
  assert.equal(await windows(plain.io), 3);
  assert.equal(plain.calls.filter((call) => call[0] === "spawn").length, 1);
  assert.equal(plain.out.stdout + plain.out.stderr, "");
  // 写了交接文件、但退出码不是 0：删掉文件，不更新，转发它的退出码。
  const failed = fakeIo({
    child: (options, files) => {
      files.set(options.env[HANDOFF_VARIABLE], '{"version":"0.1.55"}\n');
      return 2;
    },
  });
  assert.equal(await windows(failed.io), 2);
  assert.equal(failed.files.size, 0);
  assert.equal(failed.calls.filter((call) => call[0] === "spawn").length, 1);
});

test("the handoff carries only a validated version, never a command", async () => {
  for (const version of ["0.1.55", "10.20.30", "latest"]) {
    assert.equal(handoffVersion(`${JSON.stringify({ version })}\n`), version);
  }
  for (const text of [
    '{"version":"1.2"}',
    '{"version":"1.2.3 && calc"}',
    '{"version":"1.2.3\\""}',
    '{"version":"latest; del x"}',
    '{"version":"../1.2.3"}',
    '{"version":" 1.2.3"}',
    '{"version":1}',
    '{"version":null}',
    '{"command":"npm install -g evil"}',
    '["1.2.3"]',
    "1.2.3",
    "not json",
    "",
  ]) {
    assert.throws(() => handoffVersion(text), /invalid update handoff/u, text);
  }
  // 坏的交接文件：删掉，报错（入口打印 `zcode: invalid update handoff`、退出码 1），不起包管理器。
  const fake = fakeIo({
    child: (options, files) => {
      files.set(options.env[HANDOFF_VARIABLE], '{"version":"0.1.55 & calc"}\n');
      return 0;
    },
  });
  await assert.rejects(windows(fake.io), /invalid update handoff/u);
  assert.equal(fake.files.size, 0);
  assert.equal(fake.calls.filter((call) => call[0] === "spawn").length, 1);
});

test("the update command is built from the launcher's own manager and package", () => {
  assert.deepEqual(updateCommand("npm", ENTRY, "0.1.55"), [
    "npm",
    "install",
    "-g",
    "@mbearo/zcode@0.1.55",
  ]);
  assert.deepEqual(updateCommand("pnpm", ENTRY, "latest"), [
    "pnpm",
    "add",
    "-g",
    "@mbearo/zcode@latest",
  ]);
  assert.deepEqual(updateCommand("bun", ENTRY, "0.1.55"), [
    "bun",
    "install",
    "-g",
    "@mbearo/zcode@0.1.55",
  ]);
  assert.throws(() => updateCommand("npm", ENTRY, "1.2"), /invalid/u);
  assert.throws(() => updateCommand("npm", "@mbearo/zcode&x", "1.2.3"), /invalid/u);
  assert.throws(() => updateCommand("yarn", ENTRY, "1.2.3"), /invalid/u);
});

test("Windows commands are found in absolute PATH entries in PATHEXT order", async () => {
  // Windows 的文件名不分大小写；找到的路径里扩展名是 PATHEXT 的写法。
  const exists = (paths) => async (path) =>
    paths.some((item) => item.toLowerCase() === path.toLowerCase());
  // 目录按 PATH 的顺序，同一目录里按 PATHEXT 的顺序；带引号的项去掉引号；相对路径项与 `\x` 不算。
  const env = {
    PATH: 'tools;\\rooted;"C:\\Program Files\\nodejs";D:\\other',
    PATHEXT: ".EXE;.CMD",
  };
  assert.equal(
    await findWindowsCommand(
      "npm",
      env,
      exists([
        "tools\\npm.exe",
        "\\rooted\\npm.exe",
        "C:\\Program Files\\nodejs\\npm.cmd",
        "D:\\other\\npm.exe",
      ]),
    ),
    "C:\\Program Files\\nodejs\\npm.CMD",
  );
  assert.equal(
    await findWindowsCommand(
      "npm",
      env,
      exists(["C:\\Program Files\\nodejs\\npm.exe", "C:\\Program Files\\nodejs\\npm.cmd"]),
    ),
    "C:\\Program Files\\nodejs\\npm.EXE",
  );
  // 键名不分大小写；没有 PATHEXT 时用 Windows 的缺省；UNC 路径算绝对路径。
  assert.equal(
    await findWindowsCommand(
      "pnpm",
      { Path: "\\\\server\\share\\bin" },
      exists(["\\\\server\\share\\bin\\pnpm.CMD"]),
    ),
    "\\\\server\\share\\bin\\pnpm.CMD",
  );
  assert.equal(
    await findWindowsCommand("npm", { PATH: "C:\\x", PATHEXT: ".bat" }, exists(["C:\\x\\npm.bat"])),
    "C:\\x\\npm.bat",
  );
  assert.equal(
    await findWindowsCommand("npm", { PATH: "tools" }, exists(["tools\\npm.cmd"])),
    null,
  );
  assert.equal(await findWindowsCommand("npm", {}, exists([])), null);
});

test("Windows console signals leave zcode.exe running and its exit code is forwarded", async () => {
  // 以前 SIGINT 被转成 child.kill（Windows 上是 TerminateProcess），SIGBREAK、SIGHUP 让启动器先退出。
  let during;
  const fake = fakeIo({
    child: (options, files, signals) => {
      during = ["SIGINT", "SIGBREAK", "SIGHUP", "SIGTERM"].map((name) =>
        signals.listenerCount(name),
      );
      for (const name of ["SIGINT", "SIGBREAK", "SIGHUP"]) signals.emit(name, name);
      return 130;
    },
  });
  assert.equal(await windows(fake.io), 130);
  assert.deepEqual(during, [1, 1, 1, 0]);
  assert.ok(
    fake.calls.every((call) => call[0] !== "kill"),
    JSON.stringify(fake.calls),
  );
  // 结束后处理器都去掉。
  assert.deepEqual(
    ["SIGINT", "SIGBREAK", "SIGHUP"].map((name) => fake.signals.listenerCount(name)),
    [0, 0, 0],
  );
  // 子进程起不来：报错、退出码 1。
  const broken = fakeIo();
  broken.io.spawn = (...args) => {
    broken.calls.push(["spawn", ...args]);
    const child = new EventEmitter();
    setImmediate(() => child.emit("error", new Error("spawn ENOENT")));
    return child;
  };
  assert.equal(await windows(broken.io), 1);
  assert.equal(broken.out.stderr, "zcode: spawn ENOENT\n");
});
