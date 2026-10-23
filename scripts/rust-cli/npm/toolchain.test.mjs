// scripts/rust-cli/npm/toolchain.mjs 的测试：libstdc++.a 的路径与 cl 的版本怎么认。真正的探测只在 CI 的 runner 上跑，
// 本机只跑本平台的那一项。
import assert from "node:assert/strict";
import test from "node:test";
import { checkToolchain, clVersion, libstdcxxPath, supportsAvx512 } from "./toolchain.mjs";

test("libstdc++.a is found only when the compiler prints an absolute path", () => {
  assert.equal(
    libstdcxxPath("/usr/lib/gcc/x86_64-linux-gnu/11/libstdc++.a\n"),
    "/usr/lib/gcc/x86_64-linux-gnu/11/libstdc++.a",
  );
  // gcc 找不到时原样打印名字。
  assert.equal(libstdcxxPath("libstdc++.a\n"), null);
  assert.equal(libstdcxxPath(""), null);
});

test("/arch:AVX512 needs cl 19.23 (VS 2019 16.3) or newer", () => {
  const banner =
    "Microsoft (R) C/C++ Optimizing Compiler Version 19.44.35207 for x64\r\nCopyright (C) Microsoft Corporation.  All rights reserved.\r\n";
  assert.deepEqual(clVersion(banner), [19, 44, 35207]);
  assert.equal(clVersion("cl: not a banner"), null);
  assert.equal(supportsAvx512([19, 44, 35207]), true);
  assert.equal(supportsAvx512([19, 23, 28105]), true);
  assert.equal(supportsAvx512([19, 22, 27905]), false);
  assert.equal(supportsAvx512([20, 0, 0]), true);
});

test("an unknown platform has no release toolchain", async () => {
  await assert.rejects(checkToolchain("freebsd", {}), /No release toolchain for freebsd/u);
});

test("the toolchain of this machine", { skip: process.platform !== "darwin" }, async () => {
  assert.match(await checkToolchain("darwin", process.env), /^clang\+\+: \//u);
});
