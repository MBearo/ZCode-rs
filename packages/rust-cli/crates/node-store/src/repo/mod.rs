//! Node 会话库的纯 SQL 仓储：打开与迁移、会话、消息与 part、session_entry
//! （复制自 `apps/zcode-cli-rust/crates/state/src/node`，docs/specs/rust-cli/node-store.md §2）。
//!
//! 这里只有 Node 原文 SQL 与行编解码，不知道内核的事实；事实到记录的映射在 `facts`。
//! 所有函数都是同步的，只在存储的阻塞线程里调用。
pub mod codecs;
pub mod entries;
pub mod messages;
pub mod migrations;
pub mod open;
mod open_error;
pub mod sessions;
pub mod usage;

#[cfg(test)]
#[path = "repo_tests.rs"]
mod repo_tests;
