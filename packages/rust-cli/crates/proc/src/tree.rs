// 复制自 rust/crates/proc/src/tree.rs（feat/rs 79b6691c55）。
//! 结束子进程树（docs/specs/rust-u0-foundation.md §3.4、§4.1）。
//!
//! Unix：[`terminate`] 按身份（pid + 启动时间）追踪根进程的整棵树并结束它；根进程必须自成进程组
//! （[`crate::ledger::Ledger::spawn`]），进程组 ID 等于根 pid。
//! Windows：整棵树由 `crate::job::Job` 结束，没有 TERM 阶段；[`Shutdown`] 在两个平台上都可以
//! 使用，调用方不需要按平台区分。

#[cfg(unix)]
mod posix;
#[cfg(unix)]
mod table;

#[cfg(unix)]
pub use posix::terminate;
#[cfg(unix)]
pub(crate) use posix::{Adopted, terminate_adopted};

/// 结束进程树的方式。Windows 上两种方式相同：直接结束整棵树。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shutdown {
    /// 先 TERM 整棵树，宽限 1.5 s 后仍存活的成员再 KILL。用于取消、超时与主动关闭。
    Graceful,
    /// 根进程已自行退出：根进程组已不存在时直接返回，否则不发 TERM，直接 KILL 残留成员。
    Immediate,
}
