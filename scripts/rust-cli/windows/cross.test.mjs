// Windows 交叉 clippy 的脚本（docs/specs/rust-cli/performance.md §8，工作线 F66）：参数、桩工具的环境变量与包装脚本、
// 桩工具写的文件。
import assert from "node:assert/strict";
import { execFile } from "node:child_process";
import { mkdtemp, readFile, readdir, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";
import { promisify } from "node:util";

import {
  WINDOWS_TARGET,
  clippyArgs,
  parseCli,
  stubEnv,
  stubLauncher,
  stubOutputs,
} from "./cross.mjs";

const exec = promisify(execFile);

test("only --target windows is accepted", () => {
  assert.deepEqual(parseCli([]), { windows: false });
  assert.deepEqual(parseCli(["--target", "windows"]), { windows: true });
  assert.throws(() => parseCli(["--target", "linux"]), /不认识的参数/u);
  assert.throws(() => parseCli(["--clippy"]), /不认识的参数/u);
});

test("the stubs replace only the Windows target's C and C++ compilers and archiver", () => {
  assert.equal(WINDOWS_TARGET, "x86_64-pc-windows-msvc");
  assert.deepEqual(stubEnv({ cc: "/t/cc", ar: "/t/ar" }), {
    CC_x86_64_pc_windows_msvc: "/t/cc",
    CXX_x86_64_pc_windows_msvc: "/t/cc",
    AR_x86_64_pc_windows_msvc: "/t/ar",
  });
  assert.deepEqual(clippyArgs({ windows: true, features: ["--features", "parity"] }), [
    "clippy",
    "--locked",
    "--workspace",
    "--all-targets",
    "--target",
    "x86_64-pc-windows-msvc",
    "--features",
    "parity",
    "--",
    "-D",
    "warnings",
  ]);
  assert.ok(!clippyArgs({ windows: false, features: [] }).includes("--target"));
  const text = stubLauncher({ node: "/n/node", script: "/s/it's.mjs", tool: "cc" });
  assert.match(text, /^#!\/bin\/sh\n/u);
  assert.ok(text.endsWith(`exec '/n/node' '/s/it'\\''s.mjs' cc "$@"\n`), text);
});

test("the stub compiler writes the object it is asked for and nothing when probed", () => {
  // cc-rs 的 cl 写法、GNU 写法；探测编译器族（`-E`）与 cl 写法（`-?`）不写文件。
  assert.deepEqual(stubOutputs("cc", ["-nologo", "-MT", "-c", "-Fo/o/a.o", "/s/a.c"]), ["/o/a.o"]);
  assert.deepEqual(stubOutputs("cc", ["/c", "/Fo/o/b.obj", "b.c"]), ["/o/b.obj"]);
  assert.deepEqual(stubOutputs("cc", ["-c", "-o", "/o/c.o", "c.c"]), ["/o/c.o"]);
  assert.deepEqual(stubOutputs("cc", ["-E", "/t/detect.c"]), []);
  assert.deepEqual(stubOutputs("cc", ["-?"]), []);
  // `-openmp` 一类的选项不是输出。
  assert.deepEqual(stubOutputs("cc", ["-openmp", "-c", "-Fo/o/d.o", "d.c"]), ["/o/d.o"]);
  assert.deepEqual(stubOutputs("ar", ["-out:/o/libx.a", "-nologo", "/o/a.o"]), ["/o/libx.a"]);
  assert.deepEqual(stubOutputs("ar", ["/OUT:/o/liby.lib", "/o/a.o"]), ["/o/liby.lib"]);
  assert.throws(() => stubOutputs("ld", []), /不认识的工具/u);
});

test("the stub tool leaves empty outputs behind", async (t) => {
  const dir = await mkdtemp(join(tmpdir(), "rust-cli-msvc-stub-"));
  t.after(() => rm(dir, { recursive: true, force: true }));
  const script = join(import.meta.dirname, "stub-tool.mjs");
  await exec(process.execPath, [script, "cc", "-c", `-Fo${join(dir, "a.o")}`, "a.c"]);
  await exec(process.execPath, [script, "ar", `-out:${join(dir, "liba.a")}`, join(dir, "a.o")]);
  const { stdout } = await exec(process.execPath, [script, "cc", "-E", "detect.c"]);
  assert.equal(stdout, "");
  assert.deepEqual((await readdir(dir)).sort(), ["a.o", "liba.a"]);
  assert.equal(await readFile(join(dir, "liba.a"), "utf8"), "");
  await assert.rejects(exec(process.execPath, [script, "ld"]), (error) => error.code === 2);
});
