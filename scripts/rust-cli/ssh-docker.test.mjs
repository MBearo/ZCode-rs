// SSH 的 Docker 测试的准备（remote-exec.md §19.12）：有 Docker 就编产物，没有就跳过并说明原因。
import assert from "node:assert/strict";
import test from "node:test";

import { dockerTriple, sshTestEnv } from "./ssh-docker.mjs";

test("Docker 的架构对应静态 musl 的 target", () => {
  assert.equal(dockerTriple("aarch64"), "aarch64-unknown-linux-musl");
  assert.equal(dockerTriple("arm64\n"), "aarch64-unknown-linux-musl");
  assert.equal(dockerTriple("x86_64"), "x86_64-unknown-linux-musl");
  assert.equal(dockerTriple("s390x"), null);
});

test("有 Docker 时编产物并交给测试", async () => {
  const built = [];
  const result = await sshTestEnv({
    platform: "darwin",
    dockerArch: async () => "aarch64",
    build: async (triple) => {
      built.push(triple);
      return `/t/${triple}/debug/zcode-rs`;
    },
  });
  assert.deepEqual(built, ["aarch64-unknown-linux-musl"]);
  assert.deepEqual(result.env, {
    ZCODE_TEST_SSH_ARTIFACT: "/t/aarch64-unknown-linux-musl/debug/zcode-rs",
  });
});

test("没有 Docker、架构不认识、编不出来时跳过并写明原因", async () => {
  const never = async () => assert.fail("not built");
  const noDocker = await sshTestEnv({
    platform: "linux",
    dockerArch: async () => {
      throw new Error("Cannot connect to the Docker daemon\nmore");
    },
    build: never,
  });
  assert.match(noDocker.message, /SSH 测试跳过：没有可用的 Docker（Cannot connect to the Docker daemon）/);
  assert.ok(noDocker.env.ZCODE_TEST_SSH_SKIP);
  const odd = await sshTestEnv({ platform: "linux", dockerArch: async () => "s390x", build: never });
  assert.match(odd.env.ZCODE_TEST_SSH_SKIP, /s390x/);
  const broken = await sshTestEnv({
    platform: "darwin",
    dockerArch: async () => "x86_64",
    build: async () => {
      throw new Error("找不到 zig");
    },
  });
  assert.match(broken.env.ZCODE_TEST_SSH_SKIP, /找不到 zig/);
  const windows = await sshTestEnv({ platform: "win32", dockerArch: never, build: never });
  assert.match(windows.env.ZCODE_TEST_SSH_SKIP, /Windows/);
  const given = await sshTestEnv({ platform: "linux", dockerArch: never, build: never, given: "/a" });
  assert.deepEqual(given.env, { ZCODE_TEST_SSH_ARTIFACT: "/a" });
});
