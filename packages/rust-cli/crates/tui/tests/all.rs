// 改写自 apps/zcode-host/crates/tui/tests/all.rs（feat/rs 79b6691c55）。
//! 这个 crate 唯一的集成测试二进制（docs/specs/rust-engineering-practices.md §4.2）：每个测试文件是
//! `suite/` 下的一个模块。内核是 LocalKernel 加 kernel-api `testing` 的替身，终端是 ratatui 的
//! TestBackend（docs/specs/rust-cli/tui.md §8）。

#![allow(
    clippy::unwrap_used,
    reason = "集成测试：辅助函数不在 #[test] 里，clippy 的测试豁免覆盖不到"
)]

mod suite;
