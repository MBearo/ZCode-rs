//! 主题（docs/specs/rust-cli/tui-kit.md §3.4）：Rgb 与颜色算法、具名主题的数据、token、选择的读写、笔的动效。
//! 移植自 packages/tui/tui/src/zcode/tui/{Theme,Ease}.scala 与 theme/（feat/tui a2bf878b34）。
pub mod ease;
pub mod rgb;
pub mod scheme;
pub mod schemes;
pub mod theme;

pub use rgb::Rgb;

#[cfg(test)]
mod ease_tests;
#[cfg(test)]
mod schemes_tests;
#[cfg(test)]
mod theme_tests;
