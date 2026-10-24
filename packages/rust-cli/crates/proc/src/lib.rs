// 复制自 rust/crates/proc/src/lib.rs（feat/rs 79b6691c55）。
//! 子进程的唯一入口（docs/specs/rust-u0-foundation.md §3.4、§3.5）。
//!
//! 两个应用 workspace 中创建子进程只能经过 [`command`] 与 [`std_command`]：仓库根 `clippy.toml`
//! 的 `disallowed-methods` 禁止在其他位置调用 `Command::new`。本期它们不改变任何默认行为；
//! U1 起在这里注入运行时环境。
//!
//! - [`shell`]：跑一段 shell 文本（Unix `/bin/bash -c`，Windows `cmd.exe /d /s /c`）。
//! - [`ledger`]：runtime 的工具进程树（Bash、hooks、MCP stdio）的唯一启动入口：自成会话或进程组，
//!   Unix 上写账本记录，所在进程被强杀后由下一个 runtime 实例清扫；Windows 上放进随本进程结束的 Job。
//! - [`tree`]：Unix 上按身份追踪并结束整棵进程树。
//! - [`daemon`]：常驻执行器的拉起（自成会话、不写进程账本，remote-exec.md §19.5）。
//! - [`private`]：本用户私有的临时目录（`<tmp>/zcode-<uid>`），进程账本与 Bash 输出都在这里。
//! - `job`（Windows）：Job Object，整棵树随 Job 一起结束。
//! - [`process_started`]：进程的启动时间（与 PID 一起区分 PID 复用）。
//!
//! 让子进程自成会话的 `group::isolate` 不公开：自成会话的树只能经 [`ledger::Ledger::spawn`] 启动，
//! 不会漏写记录。

use std::ffi::OsStr;

pub mod daemon;
#[cfg(unix)]
mod group;
mod identity;
#[cfg(windows)]
pub mod job;
pub mod ledger;
pub mod private;
pub mod shell;
pub mod tree;

pub use identity::process_started;

/// 创建 `tokio` 子进程命令。与 `tokio::process::Command::new` 完全相同，不改变任何默认行为
/// （不改环境、不设 `kill_on_drop`）。
#[expect(
    clippy::disallowed_methods,
    reason = "zcode-proc 是唯一允许直接创建 Command 的位置（spec §3.5）"
)]
pub fn command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    tokio::process::Command::new(program)
}

/// 创建同步子进程命令，用于不在 tokio 运行时中的代码。与 `std::process::Command::new` 完全相同。
#[expect(
    clippy::disallowed_methods,
    reason = "zcode-proc 是唯一允许直接创建 Command 的位置（spec §3.5）"
)]
pub fn std_command(program: impl AsRef<OsStr>) -> std::process::Command {
    std::process::Command::new(program)
}
