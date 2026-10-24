// SSH 的 Docker 测试的准备（docs/specs/rust-cli/remote-exec.md §19.12）：本机有 Docker 时交叉编译 Docker 架构的 debug musl
// 产物，交给 cli 的 `tests/ssh_docker.rs`（环境变量 ZCODE_TEST_SSH_ARTIFACT）；没有 Docker 或编不出来时跳过并说明原因
// （ZCODE_TEST_SSH_SKIP）。debug 构建不动 build-number。

/** Docker 报告的架构 → 静态 musl 产物的 target。 */
const TRIPLES = {
  aarch64: "aarch64-unknown-linux-musl",
  arm64: "aarch64-unknown-linux-musl",
  x86_64: "x86_64-unknown-linux-musl",
  amd64: "x86_64-unknown-linux-musl",
};

export function dockerTriple(arch) {
  return TRIPLES[String(arch).trim()] ?? null;
}

/**
 * 算出给 nextest 的环境变量与要打印的一行。
 * - `platform`：process.platform；
 * - `dockerArch()`：Docker 服务端的架构（没有 Docker 时抛错）；
 * - `build(triple)`：交叉编译，返回产物路径（失败时抛错）；
 * - `given`：调用方已经给了 ZCODE_TEST_SSH_ARTIFACT 时用它。
 */
export async function sshTestEnv({ platform, dockerArch, build, given }) {
  if (given) return { env: { ZCODE_TEST_SSH_ARTIFACT: given }, message: `SSH 测试：用给定的产物 ${given}` };
  const skip = (reason) => ({
    env: { ZCODE_TEST_SSH_SKIP: reason },
    message: `SSH 测试跳过：${reason}`,
  });
  if (platform === "win32") return skip("Windows 上不跑 SSH 的 Docker 测试");
  let arch;
  try {
    arch = await dockerArch();
  } catch (error) {
    return skip(`没有可用的 Docker（${String(error.message ?? error).split("\n")[0]}）`);
  }
  const triple = dockerTriple(arch);
  if (!triple) return skip(`Docker 的架构 ${arch} 没有对应的 Linux 产物`);
  try {
    const path = await build(triple);
    return {
      env: { ZCODE_TEST_SSH_ARTIFACT: path },
      message: `SSH 测试：Docker（${arch}），产物 ${path}`,
    };
  } catch (error) {
    return skip(`交叉编译 ${triple} 失败（${String(error.message ?? error).split("\n")[0]}）`);
  }
}
