//! 渲染层（docs/specs/rust-cli/tui-kit.md §2.1）：格子、样式、宽度、ANSI 序列、样式行、样式池、格子网、渲染器与终端。
//! 移植自 packages/tui/tui/src/zcode/tui/render/（feat/tui a2bf878b34）。
pub mod ansi;
pub mod cell;
pub mod color;
pub mod line;
pub mod panic_log;
pub mod pool;
pub mod real;
pub mod renderer;
pub mod style;
pub mod surface;
pub mod terminal;
pub mod width;
mod width_tables;

pub use cell::{Cell, WidthClass};
pub use color::{Color, ColorDepth};
pub use line::{Continuation, Span, StyledLine};
pub use pool::StylePool;
pub use style::{Decor, Style, attr};
pub use surface::Surface;

#[cfg(test)]
mod color_tests;
#[cfg(test)]
mod primitives_tests;
#[cfg(test)]
mod renderer_tests;
