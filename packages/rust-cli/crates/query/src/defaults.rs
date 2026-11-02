//! 工具输入的整理与写回（docs/specs/rust-cli/query.md §3「工具输入的整理与写回」）。
//!
//! 组装 assistant 消息时按 tool_use 的工具整理输入（`Tool::rewrite_input`），结果写回 tool_use 块，执行与回放给模型的
//! 都是它。主循环不认识具体的工具（工作线 F131：以前这里按名字认 Edit，Edit 的规则在 tools 的 `edit_input.rs`）。

use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::orchestration::ToolRegistry;

/// tool_use 块按它的工具整理输入；找不到工具或工具不整理时不动（找不到的之后回填“没有这个工具”的错误）。
pub(crate) fn apply(tools: &dyn ToolRegistry, block: &mut ContentBlock) {
    if let ContentBlock::ToolUse { name, input, .. } = block
        && let Some(rebuilt) = tools.find(name).and_then(|tool| tool.rewrite_input(input))
    {
        *input = rebuilt;
    }
}

#[cfg(test)]
#[path = "defaults_tests.rs"]
mod tests;
