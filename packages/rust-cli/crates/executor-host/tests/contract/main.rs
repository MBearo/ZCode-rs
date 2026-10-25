//! 执行端口的合同测试（docs/specs/rust-cli/remote-exec.md §15.6、§18.7）：同一套测试对两种 `MachineEnv` 各跑一遍——
//! 本机的 `LocalEnvironment`，与经内存 loopback 连到同进程 exec-server（背后是同一个 `LocalEnvironment`）的
//! `RemoteEnvironment`。每个测试只经 `MachineEnv` 的端口操作，两种实现的断言完全相同。
//!
//! 测试从 env-local 原有的端口测试搬来（R1）；只测内部函数的留在 env-local。

#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：直接准备与检查工作区夹具"
)]

mod background;
mod checkpoints;
mod commands;
mod files;
#[cfg(unix)]
mod mcp;
mod pdf;
mod search;
#[cfg(unix)]
mod shell_support;
mod support;

/// 为每个合同测试生成两个用例：`local` 与 `loopback`。
#[macro_export]
macro_rules! contract {
    ($($name:ident),* $(,)?) => {
        $(
            mod $name {
                #[tokio::test(flavor = "multi_thread")]
                async fn local() {
                    super::$name($crate::support::Kind::Local).await;
                }

                #[tokio::test(flavor = "multi_thread")]
                async fn loopback() {
                    super::$name($crate::support::Kind::Loopback).await;
                }
            }
        )*
    };
}
