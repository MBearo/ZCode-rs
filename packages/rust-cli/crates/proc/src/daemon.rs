//! 常驻执行器的拉起（docs/specs/rust-cli/remote-exec.md §19.5）。
//!
//! 子进程自成会话（Unix `setsid`，同 [`crate::ledger::Isolation::Session`]），所以拉起它的代理退出、ssh 会话结束时
//! 它不会收到终端的信号。它不写进程账本：账本的清扫会结束“所有者已不在”的树，而常驻执行器本来就要比拉起它的
//! 代理活得久。只给常驻执行器用；工具的进程树仍只经 [`crate::ledger::Ledger::spawn`] 启动。

use std::io;

use tokio::process::{Child, Command};

/// 启动 `command` 作为一个脱离当前会话的常驻进程。丢掉返回的 `Child` 不会结束它。
pub fn spawn(command: &mut Command) -> io::Result<Child> {
    #[cfg(unix)]
    crate::group::isolate(command);
    command.kill_on_drop(false);
    command.spawn()
}

#[cfg(all(test, unix))]
#[path = "daemon_tests.rs"]
mod tests;
