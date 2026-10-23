// cargo 的 feature 参数（docs/specs/rust-cli/export.md §5）：parity 测试的数据目录 packages/rust-cli/tests/parity
// 在时（完整的源码仓库）打开 `parity`，不在时（只有产品代码的树）不打开。test.mjs 与 clippy.mjs 共用。
import { access } from "node:fs/promises";
import { resolve } from "node:path";

/** `workspace` 是 packages/rust-cli。 */
export async function featureArgs(workspace) {
  try {
    await access(resolve(workspace, "tests/parity"));
    return ["--features", "parity"];
  } catch {
    return [];
  }
}
