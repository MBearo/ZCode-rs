//! 记忆（docs/specs/rust-cli/memory.md）。
//!
//! 自动记忆目录所在的项目根（[`project_root`]）、`MEMORY.md` 索引的截断
//! （[`entrypoint`]）。目录在本机存储里，由入口经 `rust-cli-fs` 创建，本 crate 不做 IO。
//!
//! 未覆盖：MEMORY.md 作为 AutoMem 规则文件宣布（交给 context）、回合结束后的抽取、相关记忆召回、
//! 整理、子代理记忆（memory.md §1）。

pub mod entrypoint;
mod project;

pub use project::project_root;
