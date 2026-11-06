//! 终端界面（tui）的渲染层（docs/specs/rust-cli/tui-kit.md）：从 Scala.js 的 Puffin 移植成 Rust，
//! 各层见 docs/specs/rust-cli/tui-kit.md §2.1，界面怎么用它们见 §3。
//!
//! 结构：`render`（格子、样式、渲染器）→ `app`（Element、布局、按键、运行时）→ `markdown`、`theme`。
//! 只依赖 base，只做终端 IO。
//!
//! 没有 unsafe：workspace 的 `deny` 在这里收紧成 `forbid`，不能用 `allow` 放开。
#![forbid(unsafe_code)]

pub mod app;
pub mod markdown;
pub mod render;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod theme;
