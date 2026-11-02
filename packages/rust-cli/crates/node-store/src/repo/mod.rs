//! Node 会话库的纯 SQL 仓储：打开与迁移、会话、消息与 part、session_entry、`local_setting`、todo、用量与用量的只读报表
//! （复制自 `apps/zcode-cli-rust/crates/state/src/node`，docs/specs/rust-cli/node-store.md §1、§2；F128 起散在别处的 SQL
//! 也收在这里，F138 收齐用量报表与用量的 savepoint）。
//!
//! 这里只有 Node 原文 SQL 与行编解码，不知道内核的事实；事实到记录的映射在 `facts`。
//! 所有函数都是同步的，只在存储的阻塞线程里调用。
pub use rust_cli_node_records::codecs;
pub mod entries;
pub mod messages;
pub mod migrations;
pub mod open;
mod open_error;
pub mod sessions;
pub mod settings;
pub mod todos;
pub mod usage;
pub mod usage_reports;

#[cfg(test)]
#[path = "repo_tests.rs"]
mod repo_tests;
