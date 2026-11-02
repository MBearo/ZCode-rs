//! `zcode`：rust-cli 的组装根（docs/specs/rust-cli/README.md §3；目录见 cli-args.md §14.5）。
//!
//! 只有这个 crate 读取进程环境与 cwd，读到的值显式传给下层。`main` 先认内嵌的搜索工具，之后交给进程骨架（`process`）：
//! 按参数选一件事，命令行与会话之外的子命令在 `commands`，controller 的组装在 `assemble`，三个入口（交互界面、`-p`、
//! app-server）在 `frontends`；远端机器在 `machines`，Node 配置文件的读法在 `config`。
mod assemble;
mod commands;
mod config;
mod consts;
mod frontends;
mod machines;
mod process;

use std::process::ExitCode;

/// Linux musl 版的全局分配器（docs/specs/rust-cli/linux.md §8.3，工作线 F44）：musl 自带的 malloc 在多线程下慢，
/// Rust 代码的分配换成 mimalloc；C 代码（SQLite、ring）仍用 musl 的 malloc。macOS 与 glibc 不变。
#[cfg(all(target_os = "linux", target_env = "musl"))]
#[global_allocator]
static ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

/// 本工程的版本：唯一来源，由 build.rs 在编译期给出（release 第三位是编译次数，debug 是 Cargo.toml 的版本，
/// docs/specs/rust-cli/version.md）。运行时显式传给 TUI、会话库、MCP 握手与账号服务，其他 crate 不读包版本。
const VERSION: &str = env!(rust_cli_base::zcode_build_version_var!());

#[expect(
    clippy::disallowed_methods,
    reason = "入口 crate 是唯一读取进程参数的位置（README §4 的环境类入口）"
)]
fn args() -> Vec<String> {
    std::env::args().skip(1).collect()
}

fn main() -> ExitCode {
    // 内嵌的 rg：以工具的名字启动时直接跑它，不做 zcode 的任何初始化（embedded-search.md §3，F53）。
    if let Some(code) = commands::embedded::dispatch() {
        return code;
    }
    process::main(&args())
}
