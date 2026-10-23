// rust-cli 的可执行文件名（工作线 F59，docs/specs/rust-cli/cli-args.md §13）：用户敲的命令是 `zcode`（npm 入口包的
// bin 名，产品文字里写死），cargo 产出的可执行文件是 `zcode-rs`（Windows 是 `zcode-rs.exe`），macOS / Linux 的进程列表
// 里显示它。Windows 的平台包里是打包时复制改名的 `zcode.exe`（工作线 F71，名字由发布仓库的启动器定义），这里只管
// cargo 产物。
// 源头是 packages/rust-cli/crates/cli/Cargo.toml 的 `[[bin]] name`；脚本只从这里取，binary.test.mjs 核对两者相同、
// 别处没有写死。
import { join } from "node:path";

export const BINARY_NAME = "zcode-rs";

/** 本机可执行文件的文件名：Windows 加 `.exe`。Linux 交叉编译的产物没有后缀，传 `"linux"`。 */
export const binaryFile = (platform = process.platform) =>
  platform === "win32" ? `${BINARY_NAME}.exe` : BINARY_NAME;

/**
 * `<repoRoot>/packages/rust-cli/target/<profile>/` 下的可执行文件。`profile` 是 `debug`、`release`，或交叉编译的
 * `<triple>/release`。
 */
export const binaryPath = (repoRoot, profile = "debug", platform = process.platform) =>
  join(repoRoot, "packages/rust-cli/target", profile, binaryFile(platform));
