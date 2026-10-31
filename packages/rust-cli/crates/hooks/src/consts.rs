//! hooks 里跨模块的固定值（docs/specs/rust-cli/constants.md §2）。

/// command hook 的退出码 2：阻断（stderr 交给模型或用户，hooks.md）。
pub(crate) const BLOCKING_EXIT: i32 = 2;
