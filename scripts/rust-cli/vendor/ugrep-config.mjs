#!/usr/bin/env node
// ugrep 的 config.h（docs/specs/rust-cli/embedded-search.md §7.2）：
//   node scripts/rust-cli/vendor/ugrep-config.mjs [--check]
// 在 packages/rust-cli/target/vendor-work 下复制 packages/rust-cli/vendor/ugrep/ugrep，用 ugrep 自己的 configure（autoconf）按
// 目标族生成 config.h，写到 vendor/ugrep/config/<族>.h。库（PCRE2、zlib、bzip2、zstd、brotli）由 build.rs 从 vendor 与
// -sys crate 编，configure 找不到它们：这里用 --without-* 跑，再把对应的 HAVE_* 打开（特性串
// `-P:pcre2jit; -z:zlib,bzip2,zstd,brotli,7z`）；lzma、lz4、bzip3、Boost 不带。交叉编译时 autoconf 不跑测试程序，
// `AC_FUNC_MMAP` 得到“没有”：Linux 上 mmap 总是能用，这里打开 HAVE_MMAP（与本机 configure 的结果相同）。
// darwin 只能在 macOS 上生成（本机 clang，部署目标 11.0）；Linux 用 zig 交叉（glibc 按 2.28），x86_64 与 aarch64 的结果
// 必须相同。Windows 的 config 是手写的（ugrep 在 Windows 上不用 configure，见 config/windows.h 的说明）。
import { execFile } from "node:child_process";
import { chmod, cp, mkdir, mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { promisify } from "node:util";

const execFileAsync = promisify(execFile);
const root = resolve(import.meta.dirname, "../../..");
const vendor = join(root, "packages/rust-cli/vendor/ugrep");
// 临时目录放在 cargo 的 target 下（git 忽略、`cargo clean` 一起删），不写系统临时目录。
const scratch = join(root, "packages/rust-cli/target/vendor-work");
const zig = process.env.ZIG || "zig";

const FLAGS = [
  "--without-pcre2",
  "--without-zlib",
  "--without-bzlib",
  "--without-lzma",
  "--without-lz4",
  "--without-zstd",
  "--without-brotli",
  "--without-boost-regex",
];

/** 打开的库（与特性串一致）。HAVE_7ZIP：configure 只在找到 zlib 时检查 --disable-7zip，这里 zlib 由 build.rs 给，照找到时的结果打开。 */
const LIBRARIES = [
  "HAVE_PCRE2",
  "HAVE_LIBZ",
  "HAVE_LIBBZ2",
  "HAVE_LIBZSTD",
  "HAVE_LIBBROTLI",
  "HAVE_7ZIP",
];

const FAMILIES = {
  darwin: { host: "darwin", variants: [{ cc: ["clang"], cxx: ["clang++"] }] },
  "linux-gnu": {
    linux: true,
    variants: ["x86_64-linux-gnu.2.28", "aarch64-linux-gnu.2.28"].map((target) => ({
      triple: target.replace(/\.2\.28$/, ""),
      cc: [zig, "cc", "-target", target],
      cxx: [zig, "c++", "-target", target],
    })),
  },
  "linux-musl": {
    linux: true,
    variants: ["x86_64-linux-musl", "aarch64-linux-musl"].map((target) => ({
      triple: target,
      cc: [zig, "cc", "-target", target],
      cxx: [zig, "c++", "-target", target],
    })),
  },
};

const quote = (word) => `'${word.replaceAll("'", `'"'"'`)}'`;

async function wrapper(path, command) {
  await writeFile(path, `#!/bin/sh\nexec ${command.map(quote).join(" ")} "$@"\n`);
  await chmod(path, 0o755);
}

async function configure(family, variant, cache) {
  await mkdir(scratch, { recursive: true });
  const work = await mkdtemp(join(scratch, `ugrep-config-${family}-`));
  try {
    const tree = join(work, "ugrep");
    await cp(join(vendor, "ugrep"), tree, { recursive: true });
    // vendor 不保留可执行位：临时副本里给 configure 与辅助脚本加上。
    for (const name of await readdir(tree)) {
      if (
        [
          "configure",
          "config.guess",
          "config.sub",
          "install-sh",
          "missing",
          "compile",
          "depcomp",
          "ar-lib",
        ].includes(name)
      ) {
        await chmod(join(tree, name), 0o755);
      }
    }
    // config.status 最后把 man/ugrep.1 链接成 man/ug.1；vendor 不带手册，放一个空文件让它走完。
    await mkdir(join(tree, "man"), { recursive: true });
    await writeFile(join(tree, "man/ugrep.1"), "");
    const cc = join(work, "cc");
    const cxx = join(work, "cxx");
    await wrapper(cc, variant.cc);
    await wrapper(cxx, variant.cxx);
    const env = {
      PATH: process.env.PATH,
      HOME: work,
      MACOSX_DEPLOYMENT_TARGET: "11.0",
      ZIG_GLOBAL_CACHE_DIR: cache,
      ZIG_LOCAL_CACHE_DIR: cache,
    };
    const args = [...FLAGS, `CC=${cc}`, `CXX=${cxx}`];
    if (variant.triple) args.push(`--host=${variant.triple}`, "--build=aarch64-apple-darwin");
    await execFileAsync("./configure", args, { cwd: tree, env, maxBuffer: 64 * 1024 * 1024 });
    return await readFile(join(tree, "config.h"), "utf8");
  } finally {
    await rm(work, { recursive: true, force: true });
  }
}

/** configure 的结果 → 仓库里的文件：打开库与 Linux 的 mmap，加上来源说明。 */
export function finish(family, text, { linux }) {
  let body = text;
  const enable = (name) => {
    const off = `/* #undef ${name} */`;
    if (body.includes(off)) body = body.replace(off, `#define ${name} 1`);
    else if (!new RegExp(`^#define ${name} 1$`, "m").test(body)) {
      throw new Error(`${family}: config.h 里没有 ${name}`);
    }
  };
  for (const name of LIBRARIES) enable(name);
  if (linux) enable("HAVE_MMAP");
  for (const name of ["HAVE_LIBLZMA", "HAVE_LIBLZ4", "HAVE_LIBBZIP3", "HAVE_BOOST_REGEX"]) {
    if (new RegExp(`^#define ${name}\\b`, "m").test(body))
      throw new Error(`${family}: 不该打开 ${name}`);
  }
  return [
    `/* ugrep 7.8.4 的 config.h，目标族 ${family}（docs/specs/rust-cli/embedded-search.md §7.2）。`,
    "   由 scripts/rust-cli/vendor/ugrep-config.mjs 用 ugrep 的 configure 生成，不手改；库由 build.rs 编，这里打开。 */",
    body,
  ].join("\n");
}

if (import.meta.url === `file://${process.argv[1]}`) {
  const check = process.argv.includes("--check");
  await mkdir(scratch, { recursive: true });
  const cache = await mkdtemp(join(scratch, "zig-cache-"));
  let failed = false;
  try {
    for (const [family, options] of Object.entries(FAMILIES)) {
      if (options.host && process.platform !== options.host) {
        console.log(`${family}: 跳过（只能在 ${options.host} 上生成）`);
        continue;
      }
      const results = [];
      for (const variant of options.variants) {
        results.push(finish(family, await configure(family, variant, cache), options));
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
        await mkdir(join(vendor, "config"), { recursive: true });
        await writeFile(path, results[0]);
        console.log(`${family}: 写入 ${path.slice(root.length + 1)}`);
      }
    }
  } finally {
    await rm(cache, { recursive: true, force: true });
  }
  if (failed) process.exitCode = 1;
}
