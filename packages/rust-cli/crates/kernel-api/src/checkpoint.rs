//! 文件 checkpoint 在工具输出里的位置（docs/specs/rust-cli/session-lifecycle.md §4）。
//!
//! 写文件的工具在写入后把 artifact 引用放进 `ToolOutput.display.checkpoint`
//! （`{artifact, path, additions, deletions, machine?}`）。主循环先发 `ToolFinished`、再提交工具结果；提交方（会话
//! actor、子代理的 sink）经同一份 [`crate::run_ledger::RunLedger`] 在 `ToolFinished` 时记下，提交这条工具结果时把
//! [`Fact::CheckpointCreated`](crate::store::Fact::CheckpointCreated) 放进同一个事务，所以主会话与子会话的 checkpoint
//! 形状相同。

/// `ToolOutput.display` 里放 checkpoint 的键。
pub const DISPLAY_KEY: &str = "checkpoint";
