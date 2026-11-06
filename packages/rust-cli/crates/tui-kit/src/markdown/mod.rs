//! Markdown（docs/specs/rust-cli/tui-kit.md §3.5）：块与行内解析、增量文档、渲染成 Element、自带的高亮。
//! 移植自 packages/tui/tui/src/zcode/tui/markdown/（feat/tui a2bf878b34）。
pub mod ast;
pub mod block_parser;
pub mod document;
pub mod inline_parser;
pub mod render;
pub mod syntax;

#[cfg(test)]
mod block_parser_tests;
#[cfg(test)]
mod document_tests;
#[cfg(test)]
mod inline_parser_tests;
#[cfg(test)]
mod render_tests;
#[cfg(test)]
mod syntax_tests;
