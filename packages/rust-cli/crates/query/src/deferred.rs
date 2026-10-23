//! 发给模型的工具清单（docs/specs/rust-cli/tools-more.md §3.3）：tool search 分支与工具的排序。
//!
//! 工具按名字排序（内置在前、MCP 在后；注册表要求保持顺序时不排，F16）。启用延迟加载时只发非延迟工具与历史里已被 `tool_reference`
//! 引用过的延迟工具（后者带 `defer_loading`），并在倒数第二位放占位工具；不启用时去掉 ToolSearch、
//! 全部内联。“已引用”从历史推出，不另存状态。

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent};
use rust_cli_kernel_api::model::{ToolDefinition, ToolFlags};
use rust_cli_kernel_api::tool::{PromptContext, Tool};
use rust_cli_kernel_api::tool_search::{
    PLACEHOLDER, PLACEHOLDER_DESCRIPTION, TOOL_SEARCH, compare_names, is_deferred, referenced,
};
use serde_json::json;

pub(crate) struct WireTools {
    pub(crate) definitions: Vec<ToolDefinition>,
    pub(crate) flags: BTreeMap<String, ToolFlags>,
    /// 这一步启用了延迟加载（决定历史里的 `tool_reference` 怎样发出，见 [`tool_references`]）。
    pub(crate) active: bool,
}

/// 引用了延迟工具的 tool_result 所在的 user 消息末尾加的说明。
const TOOL_LOADED: &str = "Tool loaded.";
/// 没有启用延迟加载时，引用全部去掉后的占位。
const REMOVED_NOT_ENABLED: &str = "[Tool references removed - tool search not enabled]";
/// 引用的工具都不在了时的占位。
const REMOVED_UNAVAILABLE: &str = "[Tool references removed - tools no longer available]";

/// 发给模型前对历史里 `tool_reference` 的处理：没有启用延迟加载时去掉引用；启用时去掉已不可用的工具的引用；还有引用时在这条 user 消息
/// 末尾加 `Tool loaded.`。只改发出的内容，不改历史。
pub(crate) fn tool_references<'a>(
    message: &'a Message,
    active: bool,
    available: &[String],
) -> Cow<'a, Message> {
    let Message::User(user) = message else {
        return Cow::Borrowed(message);
    };
    let references = |content: &[ToolResultContent]| {
        content
            .iter()
            .any(|item| matches!(item, ToolResultContent::ToolReference { .. }))
    };
    let has = user.content.iter().any(
        |block| matches!(block, ContentBlock::ToolResult { content, .. } if references(content)),
    );
    if !has {
        return Cow::Borrowed(message);
    }
    let mut user = user.clone();
    for block in &mut user.content {
        let ContentBlock::ToolResult { content, .. } = block else {
            continue;
        };
        if !references(content) {
            continue;
        }
        content.retain(|item| match item {
            ToolResultContent::ToolReference { tool_name } => {
                active && available.iter().any(|name| name == tool_name)
            }
            _ => true,
        });
        if content.is_empty() {
            let text = if active {
                REMOVED_UNAVAILABLE
            } else {
                REMOVED_NOT_ENABLED
            };
            content.push(ToolResultContent::Text {
                text: text.to_owned(),
            });
        }
    }
    let still = user.content.iter().any(
        |block| matches!(block, ContentBlock::ToolResult { content, .. } if references(content)),
    );
    let noted = user
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Text { text } if text.starts_with(TOOL_LOADED)));
    if still && !noted {
        user.content.push(ContentBlock::Text {
            text: TOOL_LOADED.to_owned(),
        });
    }
    Cow::Owned(Message::User(user))
}

fn deferred_flag() -> ToolFlags {
    ToolFlags {
        defer_loading: true,
        ..ToolFlags::default()
    }
}

/// 工具清单的顺序。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ToolOrder {
    /// 按名字排序，内置在前、MCP 在后。
    ByName,
    /// 按给出的顺序（注册表 `keeps_order`：子代理点名的工具清单，agents.md §4.2）。
    AsGiven,
}

/// `supported`：模型支持延迟加载（模型卡片 `tool_search`）。`history` 是这一步发给模型的活动历史。
pub(crate) fn wire_tools<'a>(
    tools: &[Arc<dyn Tool>],
    history: impl IntoIterator<Item = &'a Message>,
    prompt: &PromptContext,
    (supported, order): (bool, ToolOrder),
) -> WireTools {
    let mut sorted: Vec<&Arc<dyn Tool>> = tools.iter().collect();
    if order == ToolOrder::ByName {
        sorted.sort_by(|left, right| {
            left.is_mcp()
                .cmp(&right.is_mcp())
                .then_with(|| compare_names(left.name(), right.name()))
        });
    }
    let active = supported
        && sorted.iter().any(|tool| tool.name() == TOOL_SEARCH)
        && sorted.iter().any(|tool| is_deferred(tool.as_ref()));
    let loaded = if active {
        referenced(history)
    } else {
        Default::default()
    };
    let mut definitions = Vec::new();
    let mut flags = BTreeMap::new();
    for tool in sorted {
        let deferred = active && is_deferred(tool.as_ref());
        let keep = if active {
            !deferred || loaded.contains(tool.name())
        } else {
            tool.name() != TOOL_SEARCH
        };
        if !keep {
            continue;
        }
        if deferred {
            flags.insert(tool.name().to_owned(), deferred_flag());
        }
        definitions.push(ToolDefinition {
            name: tool.name().to_owned(),
            description: tool.description(prompt),
            input_schema: tool.input_schema(),
            cache_control: None,
        });
    }
    // 占位工具（缺省开）：插在倒数第二位。
    if active && !definitions.iter().any(|tool| tool.name == PLACEHOLDER) {
        let at = definitions.len().saturating_sub(1);
        definitions.insert(
            at,
            ToolDefinition {
                name: PLACEHOLDER.to_owned(),
                description: PLACEHOLDER_DESCRIPTION.to_owned(),
                input_schema: json!({"type": "object", "properties": {}}),
                cache_control: None,
            },
        );
        flags.insert(PLACEHOLDER.to_owned(), deferred_flag());
    }
    WireTools {
        definitions,
        flags,
        active,
    }
}

#[cfg(test)]
#[path = "deferred_tests.rs"]
mod tests;
