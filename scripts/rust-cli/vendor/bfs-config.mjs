#!/usr/bin/env node
// bfs 的 config.h（docs/specs/rust-cli/embedded-search.md §6.2）：
//   node scripts/rust-cli/vendor/bfs-config.mjs [--check]
// 在系统临时目录里复制 packages/rust-cli/vendor/bfs/bfs，用 bfs 自己的 configure（参数：
// --enable-release --with-oniguruma --without-libselinux --without-libacl --without-liburing --without-libcap）按目标族
// 生成 gen/config.h，写到 vendor/bfs/config/<族>.h。Oniguruma 由 build.rs 从 vendor 的源码编，configure 找不到它：
// 这里先用 --without-oniguruma 跑，再把 BFS_WITH_ONIGURUMA 改成 true。Linux 两个架构各跑一次，结果必须相同。
// darwin 只能在 macOS 上生成（本机 clang，部署目标 11.0，与 Rust 的 aarch64-apple-darwin 缺省相同）；Linux 用 zig cc
// 交叉（glibc 按 2.28 生成，偏保守：新的 glibc 上只会少用几个新函数）。--check 不写文件，只比较。
import { execFile } from "node:child_process";
import { chmod, cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const root = resolve(import.meta.dirname, "../../..");
const vendor = join(root, "packages/rust-cli/vendor/bfs");
// 临时目录放在 cargo 的 target 下（git 忽略、`cargo clean` 一起删），不写系统临时目录。
const scratch = join(root, "packages/rust-cli/target/vendor-work");
const zig = process.env.ZIG || "zig";

const CONFFLAGS = [
  "--enable-release",
  "--without-oniguruma",
  "--without-libselinux",
  "--without-libacl",
  "--without-liburing",
  "--without-libcap",
];

/** 目标族 → 每个变体的编译器命令（第一个变体的结果写文件，其余必须相同）。 */
const FAMILIES = {
  darwin: { os: "Darwin", host: "darwin", compilers: [["clang"]] },
  "linux-gnu": {
    os: "Linux",
    compilers: [
      [zig, "cc", "-target", "x86_64-linux-gnu.2.28"],
      [zig, "cc", "-target", "aarch64-linux-gnu.2.28"],
    ],
  },
  "linux-musl": {
    os: "Linux",
    compilers: [
      [zig, "cc", "-target", "x86_64-linux-musl"],
      [zig, "cc", "-target", "aarch64-linux-musl"],
    ],
  },
};

const quote = (word) => `'${word.replaceAll("'", `'"'"'`)}'`;

/** 先单独编一个空程序，把 zig 的 libc 建进缓存（之后每个探测都用它）。 */
async function warm(compiler, work, env) {
  if (compiler[0] !== zig) return;
  const source = join(work, "warm.c");
  await writeFile(source, "int main(void) { return 0; }\n");
  await execFileAsync(compiler[0], [...compiler.slice(1), source, "-o", join(work, "warm")], {
    env,
  });
}

async function configure(family, compiler, cache) {
  await mkdir(scratch, { recursive: true });
  const work = await mkdtemp(join(scratch, `bfs-config-${family}-`));
  try {
    const tree = join(work, "bfs");
    await cp(join(vendor, "bfs"), tree, { recursive: true });
    // vendor 不保留可执行位（fetch.mjs 只写内容）：临时副本里给 configure 与 build/*.sh 加上。
    for (const script of [
      "configure",
      ...(await readdir(join(tree, "build"))).map((name) => `build/${name}`),
    ]) {
      if (script === "configure" || script.endsWith(".sh")) await chmod(join(tree, script), 0o755);
    }
    const cc = join(work, "cc");
    await writeFile(cc, `#!/bin/sh\nexec ${compiler.map(quote).join(" ")} "$@"\n`);
    await chmod(cc, 0o755);
    const env = {
      PATH: process.env.PATH,
      HOME: work,
      OS: FAMILIES[family].os,
      MACOSX_DEPLOYMENT_TARGET: "11.0",
      ZIG_GLOBAL_CACHE_DIR: cache,
      ZIG_LOCAL_CACHE_DIR: cache,
      // configure 缺省按 CPU 数并行跑探测；几个 zig 同时跑时偶尔卡住不动（zig 0.15.2，试过两次），zig 的变体串行跑。
      ...(compiler[0] === zig ? { MAKEFLAGS: "-j1" } : {}),
    };
    await warm(compiler, work, env);
    await execFileAsync("./configure", [...CONFFLAGS, `CC=${cc}`], {
      cwd: tree,
      env,
      maxBuffer: 64 * 1024 * 1024,
    });
    return await readFile(join(tree, "gen/config.h"), "utf8");
  } finally {
    await rm(work, { recursive: true, force: true });
  }
}

/** configure 的结果 → 仓库里的文件：去掉第一行（生成的路径），打开 Oniguruma，加上来源说明。 */
export function finish(family, text) {
  const lines = text.split("\n");
  if (lines[0].startsWith("// gen/config.h")) lines.shift();
  const body = lines.join("\n");
  const off = "#define BFS_WITH_ONIGURUMA false";
  if (!body.includes(off)) throw new Error(`${family}: config.h 里没有 ${off}`);
  return [
    `// bfs 4.1.4 的 gen/config.h，目标族 ${family}（docs/specs/rust-cli/embedded-search.md §6.2）。`,
    "// 由 scripts/rust-cli/vendor/bfs-config.mjs 用 bfs 的 configure 生成，不手改；Oniguruma 由 build.rs 编，这里打开。",
    body.replace(off, "#define BFS_WITH_ONIGURUMA true"),
  ].join("\n");
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const check = process.argv.includes("--check");
  // 整次运行共用一份 zig 缓存（各变体的 libc 只建一次）。
  await mkdir(scratch, { recursive: true });
  const cache = await mkdtemp(join(scratch, "zig-cache-"));
  let failed = false;
  for (const [family, { host, compilers }] of Object.entries(FAMILIES)) {
    if (host && process.platform !== host) {
      console.log(`${family}: 跳过（只能在 ${host} 上生成）`);
      continue;
    }
    const results = [];
    for (const compiler of compilers) {
      results.push(finish(family, await configure(family, compiler, cache)));
    }
    if (results.some((text) => text !== results[0])) {
      throw new Error(`${family}: 不同架构生成的 config.h 不同，需要按架构分开`);
    }
    const path = join(vendor, "config", `${family}.h`);
    if (check) {
      const current = await readFile(path, "utf8").catch(() => "");
      if (current !== results[0]) {
        failed = true;
        console.error(`${family}: 与 configure 的结果不同`);
      } else console.log(`${family}: OK`);
    } else {
      await writeFile(path, results[0]);
      console.log(`${family}: 写入 ${path.slice(root.length + 1)}`);
    }
  }
  await rm(cache, { recursive: true, force: true });
  if (failed) process.exitCode = 1;
}
