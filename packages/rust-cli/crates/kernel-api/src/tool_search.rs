//! 延迟工具与“模型这一步看到哪些工具”的公共规则（docs/specs/rust-cli/tools-more.md §3）。
//!
//! 主循环（发给模型的工具清单）、上下文（`deferred_tools_delta` 清单）、ToolSearch 工具、`system/init` 的工具清单与
//! `/context` 共用这里的判定，几处对“哪些工具可见、怎样排序、哪些延迟、哪些已被引用”的看法总是一致。只有纯函数，
//! 没有状态：已被引用的工具从历史里的 `tool_reference` 推出。

use std::cmp::Ordering;
use std::collections::BTreeSet;
use std::sync::Arc;

use crate::message::{ContentBlock, Message, SystemMessage, ToolResultContent};
use crate::provider::ModelCapabilities;
use crate::tool::Tool;
use crate::tool_names::{TOOL_SEARCH, WAIT_FOR_MCP_SERVERS};

/// 占位工具：没有已加载的延迟工具时，它让请求里始终有一个 `defer_loading` 的工具。
pub const PLACEHOLDER: &str = "DeferredToolPlaceholder";

/// 占位工具的描述。
pub const PLACEHOLDER_DESCRIPTION: &str =
    "Reserved placeholder that keeps deferred tool loading active; never call this tool.";

/// MCP 工具（[`Tool::mcp_info`] 有值）。工作线 F131 删掉了 `Tool::is_mcp` 与 `Tool::always_load`：两者都从
/// `mcp_info` 推出，以前由 ext 的 MCP 工具另外覆盖，三个方法要一起改。
pub fn is_mcp(tool: &dyn Tool) -> bool {
    tool.mcp_info().is_some()
}

/// 这个工具是否延迟：ToolSearch 自己从不延迟；MCP 工具除 `alwaysLoad`（服务器配置或工具 `_meta` 的不延迟标记，
/// ext.md §3.1）外一律延迟；其余看 `shouldDefer`。
pub fn is_deferred(tool: &dyn Tool) -> bool {
    if tool.name() == TOOL_SEARCH {
        return false;
    }
    match tool.mcp_info() {
        Some(info) => !info.always_load,
        None => tool.should_defer(),
    }
}

/// 这一步模型看得到的工具（docs/specs/rust-cli/query.md §1.3、tools-more.md §3.3）：启用的；去掉生效配置不支持的
/// WebSearch（要 `supportsNativeWebSearch`，修复线 F23，tools-more.md §5；能力答不出时不去掉）；有 ToolSearch 时去掉
/// WaitForMcpServers（§3.3 第 4 条，乐观判断：`ENABLE_TOOL_SEARCH` 为关时 ToolSearch 不启用、不在这里）。
///
/// 主循环每一步的工具快照、`system/init` 的工具清单与 `/context` 共用这一份（工作线 F131：以前三处各写一份，
/// 靠注释“同主循环”保持一致）。
pub fn visible_tools(
    tools: Vec<Arc<dyn Tool>>,
    capabilities: Option<&ModelCapabilities>,
) -> Vec<Arc<dyn Tool>> {
    let web_search = capabilities.is_none_or(|found| found.supports_native_web_search);
    let mut tools: Vec<_> = tools
        .into_iter()
        .filter(|tool| tool.is_enabled() && (web_search || !tool.needs_native_web_search()))
        .collect();
    if tools.iter().any(|tool| tool.name() == TOOL_SEARCH) {
        tools.retain(|tool| tool.name() != WAIT_FOR_MCP_SERVERS);
    }
    tools
}

/// 有它（启用的）时文件名与内容搜索交给它的工具（docs/specs/rust-cli/tools-files.md §1.1）。
const BASH: &str = crate::tool_names::BASH;

/// 有 Bash 时藏起来的搜索工具；补回时按这个顺序。
pub const SEARCH_TOOLS: [&str; 2] = [crate::tool_names::GLOB, crate::tool_names::GREP];

/// 这些工具里有启用的 Bash：Glob / Grep 交给它、不出现（tools-files.md §1.1）。
///
/// 基础注册表（藏）、整个工具的 deny 规则（Bash 被 deny 时补回）与子代理的工具清单（点名、又没有 Bash 时补回）
/// 三处用这一条（工作线 F132 的 T2：以前三处各写一份“有没有启用的 Bash”，各有自己的名字常量）。
pub fn search_covered<'a>(tools: impl IntoIterator<Item = &'a Arc<dyn Tool>>) -> bool {
    tools
        .into_iter()
        .any(|tool| tool.name() == BASH && tool.is_enabled())
}

/// 搜索回退：`shown` 里没有启用的 Bash 时，`wanted` 里的 Glob / Grep（按 `wanted` 的顺序，已在 `shown` 里或已取回的
/// 跳过）从 `withheld` 取回；有 Bash 时为空。`wanted` 由调用方先按自己的规则筛过（没被 deny、子代理允许）。
pub fn search_fallback<'a>(
    shown: &[Arc<dyn Tool>],
    wanted: impl IntoIterator<Item = &'a str>,
    withheld: impl Fn(&str) -> Option<Arc<dyn Tool>>,
) -> Vec<Arc<dyn Tool>> {
    if search_covered(shown) {
        return Vec::new();
    }
    let mut restored: Vec<Arc<dyn Tool>> = Vec::new();
    for name in wanted {
        if !SEARCH_TOOLS.contains(&name)
            || shown
                .iter()
                .chain(&restored)
                .any(|tool| tool.name() == name)
        {
            continue;
        }
        restored.extend(withheld(name));
    }
    restored
}

/// ToolSearch 用得上：工具里有它，也有延迟的工具。用不上时去掉 ToolSearch、其余全部内联（tools-more.md §3.3）；
/// 这一步开不开（`ENABLE_TOOL_SEARCH` 与自动门槛）由调用方另外判定。
pub fn search_applies(tools: &[Arc<dyn Tool>]) -> bool {
    tools.iter().any(|tool| tool.name() == TOOL_SEARCH)
        && tools.iter().any(|tool| is_deferred(tool.as_ref()))
}

/// 发给模型的工具顺序：内置在前、MCP 在后，同组按名字（[`compare_names`]）。工具顺序模型
/// 可见；主循环（注册表不要求保持顺序时）、`system/init` 与 `/context` 共用这一份（工作线 F131）。
pub fn model_order(left: &dyn Tool, right: &dyn Tool) -> Ordering {
    is_mcp(left)
        .cmp(&is_mcp(right))
        .then_with(|| compare_names(left.name(), right.name()))
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

/// 已加载的工具：[`referenced`] 加上历史里压缩边界带着的（压缩前已加载的，docs/specs/rust-cli/compact.md §3.5）。
/// 调用方传活动历史（从最后一个边界开始，含边界）。发给模型的工具清单、`/context` 的计数与压缩都用它。
pub fn loaded<'a>(history: impl IntoIterator<Item = &'a Message>) -> BTreeSet<String> {
    let history: Vec<&Message> = history.into_iter().collect();
    let mut names = referenced(history.iter().copied());
    for message in history {
        if let Message::System(SystemMessage::CompactBoundary(boundary)) = message {
            names.extend(boundary.discovered_tools.iter().cloned());
        }
    }
    names
}

#[cfg(test)]
#[path = "tool_search_tests.rs"]
mod tests;
