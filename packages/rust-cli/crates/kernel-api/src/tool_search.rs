//! 延迟工具的公共规则（docs/specs/rust-cli/tools-more.md §3）。
//!
//! 主循环（发给模型的工具清单）、上下文（`deferred_tools_delta` 清单）与 ToolSearch 工具共用这里的
//! 判定，三处对“哪些工具延迟、哪些已被引用”的看法总是一致。只有纯函数，没有状态：已被引用的工具
//! 从历史里的 `tool_reference` 推出。

use std::cmp::Ordering;
use std::collections::BTreeSet;

use crate::message::{ContentBlock, Message, ToolResultContent};
use crate::tool::Tool;

/// ToolSearch 工具的名字。
pub const TOOL_SEARCH: &str = "ToolSearch";

/// 占位工具：没有已加载的延迟工具时，它让请求里始终有一个 `defer_loading` 的工具。
pub const PLACEHOLDER: &str = "DeferredToolPlaceholder";

/// 占位工具的描述。
pub const PLACEHOLDER_DESCRIPTION: &str =
    "Reserved placeholder that keeps deferred tool loading active; never call this tool.";

/// 这个工具是否延迟：
/// `alwaysLoad` 优先；ToolSearch 自己从不延迟；MCP 工具一律延迟；其余看 `shouldDefer`。
pub fn is_deferred(tool: &dyn Tool) -> bool {
    if tool.always_load() || tool.name() == TOOL_SEARCH {
        return false;
    }
    tool.is_mcp() || tool.should_defer()
}

/// 工具名的顺序（JS `localeCompare` 的排序）：不分大小写比较，相同时按原文。
/// 发给模型的工具清单与 ToolSearch 的候选按它排；延迟工具清单（`deferred_tools_delta`）不用它，用的是
/// JS 缺省排序（码元序，docs/specs/rust-cli/tools-more.md D8）。
pub fn compare_names(left: &str, right: &str) -> Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| left.cmp(right))
}

/// 历史里被 `tool_reference` 引用过的工具名。调用方传活动历史。
pub fn referenced<'a>(history: impl IntoIterator<Item = &'a Message>) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for message in history {
        let Message::User(user) = message else {
            continue;
        };
        for block in &user.content {
            let ContentBlock::ToolResult { content, .. } = block else {
                continue;
            };
            for item in content {
                if let ToolResultContent::ToolReference { tool_name } = item {
                    names.insert(tool_name.clone());
                }
            }
        }
    }
    names
}

#[cfg(test)]
#[path = "tool_search_tests.rs"]
mod tests;
