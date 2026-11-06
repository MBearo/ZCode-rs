//! 应用层（docs/specs/rust-cli/tui-kit.md §2.1）：Element DSL 与布局、按键解析与握手、Cmd / Sub / App 与运行时。
//! 移植自 packages/tui/tui/src/zcode/tui/app/（feat/tui a2bf878b34）。
pub mod cmd;
pub mod element;
pub mod frame;
pub mod handshake;
pub mod key;
pub mod key_parser;
pub mod layout;
pub mod logger;
pub mod runtime;
pub mod screen;
pub mod select_units;
pub mod sub;
pub mod table_layout;
pub mod widgets;

#[cfg(test)]
mod handshake_tests;
#[cfg(test)]
mod key_parser_tests;
#[cfg(test)]
mod layout_tests;
#[cfg(test)]
mod runtime_tests;
#[cfg(test)]
mod table_layout_tests;
