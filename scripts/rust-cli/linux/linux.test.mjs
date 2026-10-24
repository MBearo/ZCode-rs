// Linux 交叉编译脚本的测试（docs/specs/rust-cli/linux.md §3、§4）：
//   node --test scripts/rust-cli/linux/linux.test.mjs
// 只测纯函数：参数改写、response file、环境变量与包装脚本、ELF 检查。ELF 用合成的字节，不依赖真实的构建。
import assert from "node:assert/strict";
import { test } from "node:test";

import { checkArtifact, hasAllocator, hostPathHits, inspectElf } from "./elf.mjs";
import { formatResponseFile, parseResponseFile, rewriteCcArgs } from "./linker-args.mjs";
import {
  TARGETS,
  cargoArgs,
  cargoEnv,
  launcher,
  parseCli,
  remapFlags,
  rustflagsConfig,
} from "./toolchain.mjs";

const SC =
  "/Users/dev/.rustup/toolchains/1.95.0-aarch64-apple-darwin/lib/rustlib/x86_64-unknown-linux-musl/lib/self-contained";

test("链接参数：去掉 rustc 的启动文件、-lc、-static-pie 与 --target=，其余原样", () => {
  const args = [
    "--target=x86_64-unknown-linux-musl",
    "-m64",
    `${SC}/rcrt1.o`,
    `${SC}/crti.o`,
    `${SC}/crtbeginS.o`,
    "/work/target/release/deps/zcode_rs-1.zcode_rs.abc-cgu.0.rcgu.o",
    "/work/target/release/deps/crt-helper-1.o",
    "-Wl,-Bstatic",
    "-lunwind",
    "-lc",
    "-lcompiler",
    "-L",
    SC,
    "-static-pie",
    "-static",
    "-nostartfiles",
    `${SC}/crtendS.o`,
    `${SC}/crtn.o`,
  ];
  assert.deepEqual(rewriteCcArgs(args), [
    "-m64",
    "/work/target/release/deps/zcode_rs-1.zcode_rs.abc-cgu.0.rcgu.o",
    "/work/target/release/deps/crt-helper-1.o",
    "-Wl,-Bstatic",
    "-lunwind",
    "-lcompiler",
    "-L",
    SC,
    "-static",
    "-nostartfiles",
  ]);
});

test("链接参数：Windows 主机上的反斜杠路径同样认出启动文件", () => {
  const dir =
    "C:\\Users\\dev\\.rustup\\toolchains\\x\\lib\\rustlib\\x86_64-unknown-linux-musl\\lib\\self-contained";
  assert.deepEqual(rewriteCcArgs([`${dir}\\crt1.o`, `${dir}\\Scrt1.o`, "-c", "a.c"]), [
    "-c",
    "a.c",
  ]);
});

test("编译参数：cc-rs 编 C 的命令只去掉 --target=", () => {
  const args = [
    "--target=aarch64-unknown-linux-musl",
    "-O3",
    "-ffunction-sections",
    "-fPIC",
    "-c",
    "x.c",
    "-o",
    "x.o",
  ];
  assert.deepEqual(rewriteCcArgs(args), args.slice(1));
});

test("response file：按 GNU 的写法读（反斜杠转义、引号、空白分隔）", () => {
  const text = '-m64\n/a\\ b/rcrt1.o\nC:\\\\dir\\\\x.o\n\'q r\'\n"s \\"t\\""\n\n';
  assert.deepEqual(parseResponseFile(text), [
    "-m64",
    "/a b/rcrt1.o",
    "C:\\dir\\x.o",
    "q r",
    's "t"',
  ]);
});

test("response file：写成 zig 读的写法（双引号；引号与紧挨引号的反斜杠加倍转义）", () => {
  assert.equal(
    formatResponseFile(["-m64", "a b", 'x"y', "C:\\dir\\", "p\\q", 'e\\"f']),
    '"-m64"\n"a b"\n"x\\"y"\n"C:\\dir\\\\"\n"p\\q"\n"e\\\\\\"f"\n',
  );
});

test("环境变量：按 target 设 cc-rs 的 `CC_<target>` / `AR_<target>` 与 cargo 的链接器，zig 缓存放在 target 目录", () => {
  assert.deepEqual(
    cargoEnv({
      triple: "aarch64-unknown-linux-musl",
      cc: "/w/target/zig/aarch64-unknown-linux-musl/cc",
      cxx: "/w/target/zig/aarch64-unknown-linux-musl/c++",
      ar: "/w/target/zig/aarch64-unknown-linux-musl/ar",
      cache: "/w/target/zig/cache",
    }),
    {
      CC_aarch64_unknown_linux_musl: "/w/target/zig/aarch64-unknown-linux-musl/cc",
      CXX_aarch64_unknown_linux_musl: "/w/target/zig/aarch64-unknown-linux-musl/c++",
      CXXSTDLIB_aarch64_unknown_linux_musl: "c++",
      AR_aarch64_unknown_linux_musl: "/w/target/zig/aarch64-unknown-linux-musl/ar",
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER: "/w/target/zig/aarch64-unknown-linux-musl/cc",
      CFLAGS_aarch64_unknown_linux_musl: "-DMI_LIBC_MUSL=1",
      ZIG_GLOBAL_CACHE_DIR: "/w/target/zig/cache",
      ZIG_LOCAL_CACHE_DIR: "/w/target/zig/cache",
    },
  );
});

test("路径改写：CARGO_HOME 与 rust-src 换成固定前缀，写成 TOML 数组（空格与反斜杠不拆错）", () => {
  const flags = remapFlags({
    cargoHome: "/Users/dev/.cargo",
    sysroot: "/Users/dev/.rustup/toolchains/1.95.0-aarch64-apple-darwin",
    commit: "59807616e1fa2540724bfbac14d7976d7e4a3860",
  });
  assert.deepEqual(flags, [
    "--remap-path-prefix=/Users/dev/.cargo=/cargo",
    "--remap-path-prefix=/Users/dev/.rustup/toolchains/1.95.0-aarch64-apple-darwin/lib/rustlib/src/rust=/rustc/59807616e1fa2540724bfbac14d7976d7e4a3860",
  ]);
  assert.equal(
    rustflagsConfig("x86_64-unknown-linux-musl", [
      "--remap-path-prefix=C:\\Users\\A B\\.cargo=/cargo",
    ]),
    'target.x86_64-unknown-linux-musl.rustflags=["--remap-path-prefix=C:\\\\Users\\\\A B\\\\.cargo=/cargo"]',
  );
});

test("包装脚本：Unix 是 sh，路径按单引号转义；Windows 是 .cmd", () => {
  const unix = launcher({
    windows: false,
    node: "/usr/local/bin/node",
    script: "/w/it's/zig-tool.mjs",
    zig: "/opt/homebrew/bin/zig",
    tool: "cc",
    zigTarget: "x86_64-linux-musl",
  });
  assert.equal(unix.name, "cc");
  assert.equal(
    unix.text.split("\n").at(-2),
    `exec '/usr/local/bin/node' '/w/it'\\''s/zig-tool.mjs' '/opt/homebrew/bin/zig' cc x86_64-linux-musl "$@"`,
  );
  assert.ok(unix.text.startsWith("#!/bin/sh\n"));
  const windows = launcher({
    windows: true,
    node: "C:\\node\\node.exe",
    script: "C:\\w\\zig-tool.mjs",
    zig: "C:\\zig\\zig.exe",
    tool: "ar",
    zigTarget: "aarch64-linux-musl",
  });
  assert.equal(windows.name, "ar.cmd");
  assert.equal(
    windows.text.split("\r\n").at(-2),
    '@"C:\\node\\node.exe" "C:\\w\\zig-tool.mjs" "C:\\zig\\zig.exe" ar aarch64-linux-musl %*',
  );
});

test("命令行：缺省两个 target；--target 可以写多次；--clippy；--debug；不认识的参数报错", () => {
  assert.deepEqual(parseCli([]), { targets: Object.keys(TARGETS), clippy: false, debug: false });
  assert.deepEqual(parseCli(["--target", "aarch64-unknown-linux-musl", "--clippy"]), {
    targets: ["aarch64-unknown-linux-musl"],
    clippy: true,
    debug: false,
  });
  assert.deepEqual(parseCli(["--debug", "--target", "aarch64-unknown-linux-musl"]), {
    targets: ["aarch64-unknown-linux-musl"],
    clippy: false,
    debug: true,
  });
  assert.throws(() => parseCli(["--debug", "--clippy"]), /--debug/);
  assert.throws(
    () => parseCli(["--target", "x86_64-unknown-linux-gnu"]),
    /x86_64-unknown-linux-gnu/,
  );
  assert.throws(() => parseCli(["--target"]), /--target/);
  assert.throws(() => parseCli(["--release"]), /--release/);
});

test("cargo 参数：debug 构建没有 --release（remote-exec.md §19.12）", () => {
  const config = "target.x.rustflags=[]";
  const args = cargoArgs({ triple: "x", clippy: false, debug: true, manifest: "m/Cargo.toml", config });
  assert.equal(args.includes("--release"), false);
  assert.deepEqual(args.slice(0, 2), ["build", "--locked"]);
  assert.deepEqual(args.slice(-2), ["--config", "profile.dev.debug=0"]);
});

test("cargo 参数：release 只构建 zcode-rs；clippy 查整个 workspace 与全部目标", () => {
  const config = "target.x.rustflags=[]";
  assert.deepEqual(cargoArgs({ triple: "x", clippy: false, manifest: "m/Cargo.toml", config }), [
    "build",
    "--locked",
    "--release",
    "--manifest-path",
    "m/Cargo.toml",
    "-p",
    "rust-cli",
    "--bin",
    "zcode-rs",
    "--target",
    "x",
    "--config",
    config,
  ]);
  assert.deepEqual(cargoArgs({ triple: "x", clippy: true, manifest: "m/Cargo.toml", config }), [
    "clippy",
    "--locked",
    "--manifest-path",
    "m/Cargo.toml",
    "--workspace",
    "--all-targets",
    "--target",
    "x",
    "--config",
    config,
    "--",
    "-D",
    "warnings",
  ]);
});

// ---- ELF ----

/** 合成一个 64 位小端的 ELF：程序头、`.dynamic` 与字符串表都在第一个 PT_LOAD 里（vaddr = offset + base）。 */
function elf({
  type = 2,
  machine = 62,
  interp,
  needed = [],
  dynamic = needed.length > 0,
  base = 0x400000,
}) {
  const strings = ["", ...needed];
  const strtab = Buffer.from(strings.join("\0") + "\0");
  const offsets = [];
  let at = 0;
  for (const name of strings) {
    offsets.push(at);
    at += Buffer.byteLength(name) + 1;
  }
  const phdrs = [{ type: 1 }];
  if (interp !== undefined) phdrs.push({ type: 3 });
  if (dynamic) phdrs.push({ type: 2 });
  const phoff = 64;
  const dataStart = phoff + phdrs.length * 56;
  const interpBytes = Buffer.from(`${interp ?? ""}\0`);
  const interpAt = dataStart;
  const strtabAt = interpAt + interpBytes.length;
  const dynAt = strtabAt + strtab.length;
  const entries = [...needed.map((_, i) => [1, offsets[i + 1]]), [5, base + strtabAt], [0, 0]];
  const size = dynAt + entries.length * 16;
  const buf = Buffer.alloc(size);
  buf.write("\x7fELF", 0, "latin1");
  buf[4] = 2;
  buf[5] = 1;
  buf[6] = 1;
  buf.writeUInt16LE(type, 16);
  buf.writeUInt16LE(machine, 18);
  buf.writeBigUInt64LE(BigInt(phoff), 32);
  buf.writeUInt16LE(56, 54);
  buf.writeUInt16LE(phdrs.length, 56);
  phdrs.forEach((ph, i) => {
    const o = phoff + i * 56;
    buf.writeUInt32LE(ph.type, o);
    const [offset, filesz] = {
      1: [0, size],
      2: [dynAt, entries.length * 16],
      3: [interpAt, interpBytes.length],
    }[ph.type];
    buf.writeBigUInt64LE(BigInt(offset), o + 8);
    buf.writeBigUInt64LE(BigInt(base + offset), o + 16);
    buf.writeBigUInt64LE(BigInt(filesz), o + 32);
    buf.writeBigUInt64LE(BigInt(filesz), o + 40);
  });
  interpBytes.copy(buf, interpAt);
  strtab.copy(buf, strtabAt);
  entries.forEach(([tag, value], i) => {
    buf.writeBigInt64LE(BigInt(tag), dynAt + i * 16);
    buf.writeBigUInt64LE(BigInt(value), dynAt + i * 16 + 8);
  });
  return buf;
}

test("ELF：静态可执行文件没有解释器与共享库", () => {
  assert.deepEqual(inspectElf(elf({})), {
    bits: 64,
    endian: "little",
    type: "EXEC",
    machine: 62,
    interpreter: null,
    needed: [],
  });
});

test("ELF：static-pie 有 .dynamic 但没有 DT_NEEDED", () => {
  const info = inspectElf(elf({ type: 3, machine: 183, dynamic: true }));
  assert.equal(info.type, "DYN");
  assert.equal(info.machine, 183);
  assert.deepEqual(info.needed, []);
  assert.equal(info.interpreter, null);
});

test("ELF：动态链接的读出解释器与共享库的名字", () => {
  const info = inspectElf(
    elf({ type: 3, interp: "/lib/ld-musl-x86_64.so.1", needed: ["libc.so", "libgcc_s.so.1"] }),
  );
  assert.equal(info.interpreter, "/lib/ld-musl-x86_64.so.1");
  assert.deepEqual(info.needed, ["libc.so", "libgcc_s.so.1"]);
});

test("ELF：不是 ELF、32 位或大端时报错", () => {
  assert.throws(() => inspectElf(Buffer.from("\xcf\xfa\xed\xfe0000000000000000", "latin1")), /ELF/);
  const b32 = elf({});
  b32[4] = 1;
  assert.throws(() => inspectElf(b32), /64/);
  const be = elf({});
  be[5] = 2;
  assert.throws(() => inspectElf(be), /little/);
});

test("本机路径：按字节数出现次数，并给出周围的可打印文字", () => {
  const buf = Buffer.concat([
    Buffer.from([0, 1, 2]),
    Buffer.from("panic at /Users/dev/.cargo/registry/src/x/lib.rs"),
    Buffer.from([0]),
    Buffer.from("/Users/dev/.cargo/y.rs"),
    Buffer.from([0, 0xff]),
  ]);
  assert.deepEqual(hostPathHits(buf, ["/Users/", "/Volumes/", "/Users/"]), [
    { needle: "/Users/", count: 2, sample: "panic at /Users/dev/.cargo/registry/src/x/lib.rs" },
  ]);
});

test("本机路径：程序自己的字面量（找 shell 的候选目录 /opt/homebrew/bin）不算，别的 /opt/homebrew 照算", () => {
  const buf = Buffer.concat([
    Buffer.from("/usr/local/bin/opt/homebrew/bin/bin/usr/bin"),
    Buffer.from([0]),
    Buffer.from("/opt/homebrew/Cellar/zig/0.15.2/lib/zig/libc"),
  ]);
  assert.deepEqual(hostPathHits(buf, ["/opt/homebrew"], ["/opt/homebrew/bin"]), [
    { needle: "/opt/homebrew", count: 1, sample: "/opt/homebrew/Cellar/zig/0.15.2/lib/zig/libc" },
  ]);
  assert.deepEqual(hostPathHits(buf.subarray(0, 44), ["/opt/homebrew"], ["/opt/homebrew/bin"]), []);
});

test("分配器：产物里有 mimalloc 的字符串（linux.md §8.3）", () => {
  assert.equal(hasAllocator(Buffer.from("\0\0mimalloc: warning: %s\0")), true);
  assert.equal(hasAllocator(Buffer.from("malloc: \0musl")), false);
});

test("产物检查：架构、解释器、共享库、本机路径，各报一条", () => {
  const ok = {
    bits: 64,
    endian: "little",
    type: "EXEC",
    machine: 62,
    interpreter: null,
    needed: [],
  };
  assert.deepEqual(
    checkArtifact({
      elf: ok,
      hits: [],
      machine: TARGETS["x86_64-unknown-linux-musl"].machine,
      allocator: true,
    }),
    [],
  );
  assert.deepEqual(checkArtifact({ elf: ok, hits: [], machine: 62, allocator: false }), [
    "没有 mimalloc（linux.md §8.3：musl 版的全局分配器）",
  ]);
  const problems = checkArtifact({
    elf: { ...ok, machine: 183, interpreter: "/lib/ld-musl-aarch64.so.1", needed: ["libc.so"] },
    hits: [{ needle: "/Users/", count: 3, sample: "/Users/dev/x" }],
    machine: 62,
    allocator: true,
  });
  assert.equal(problems.length, 4);
  assert.match(problems[0], /183/);
  assert.match(problems[1], /ld-musl/);
  assert.match(problems[2], /libc\.so/);
  assert.match(problems[3], /\/Users\/.*3/);
});
