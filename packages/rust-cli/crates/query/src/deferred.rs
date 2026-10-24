//! 发给模型的工具清单（docs/specs/rust-cli/tools-more.md §3.3）：tool search 分支与工具的排序。
//!
//! 工具按名字排序（内置在前、MCP 在后；注册表要求保持顺序时不排，F16）。ToolSearch 开着时只发非延迟工具与已加载的
//! 延迟工具：原生时后者带 `defer_loading`，并在倒数第二位放占位工具；本地时（工作线 F89 起产品路径一律本地，§3.2.2）
//! 都不带，历史里的引用换成文字。不开时去掉 ToolSearch、全部内联。“已加载”从历史推出（引用过的，加上压缩边界
//! 带的），不另存状态。这一步的形式见 [`step_tool_search`]（§3.2）。

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_kernel_api::message::{ContentBlock, Message, ToolResultContent, tool_reference_text};
use rust_cli_kernel_api::model::{ToolDefinition, ToolFlags};
use rust_cli_kernel_api::provider::ModelCapabilities;
use rust_cli_kernel_api::tool::{PromptContext, Tool};
use rust_cli_kernel_api::tool_search::{
    PLACEHOLDER, PLACEHOLDER_DESCRIPTION, TOOL_SEARCH, compare_names, is_deferred, loaded,
};
use rust_cli_prompts::models::{self, ToolSearchMode, ToolSearchSetting};
use serde_json::json;

pub(crate) struct WireTools {
    pub(crate) definitions: Vec<ToolDefinition>,
    pub(crate) flags: BTreeMap<String, ToolFlags>,
    /// 这一步实际的 ToolSearch 形式（没有 ToolSearch 或没有延迟工具时是关）：决定历史里的 `tool_reference` 怎样
    /// 发出，见 [`tool_references`]。
    pub(crate) mode: ToolSearchMode,
}

/// 引用了延迟工具的 tool_result 所在的 user 消息末尾加的说明。
const TOOL_LOADED: &str = "Tool loaded.";
/// 没有启用延迟加载时，引用全部去掉后的占位。
const REMOVED_NOT_ENABLED: &str = "[Tool references removed - tool search not enabled]";
/// 引用的工具都不在了时的占位。
const REMOVED_UNAVAILABLE: &str = "[Tool references removed - tools no longer available]";

/// 发给模型前对历史里 `tool_reference` 的处理：不开时去掉引用；开着时去掉已不可用的工具的引用，本地时把留下的换成文字
/// `Tool loaded: <name>`（工作线 F84）；还有引用（块或换成的文字）时在这条 user 消息末尾加 `Tool loaded.`。只改发出的
/// 内容，不改历史。
pub(crate) fn tool_references<'a>(
    message: &'a Message,
    mode: ToolSearchMode,
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
    let mut kept = false;
    for block in &mut user.content {
        let ContentBlock::ToolResult { content, .. } = block else {
            continue;
        };
        if !references(content) {
            continue;
        }
        let items = std::mem::take(content);
        for item in items {
            let ToolResultContent::ToolReference { tool_name } = item else {
                content.push(item);
                continue;
            };
            if !mode.is_on() || !available.contains(&tool_name) {
                continue;
            }
            kept = true;
            content.push(match mode {
                ToolSearchMode::Local => ToolResultContent::Text {
                    text: tool_reference_text(&tool_name),
                },
                ToolSearchMode::Native | ToolSearchMode::Off => {
                    ToolResultContent::ToolReference { tool_name }
                }
            });
        }
        if content.is_empty() {
            let text = if mode.is_on() {
                REMOVED_UNAVAILABLE
            } else {
                REMOVED_NOT_ENABLED
            };
            content.push(ToolResultContent::Text {
                text: text.to_owned(),
            });
        }
    }
    let noted = user
        .content
        .iter()
        .any(|block| matches!(block, ContentBlock::Text { text } if text.starts_with(TOOL_LOADED)));
    if kept && !noted {
        user.content.push(ContentBlock::Text {
            text: TOOL_LOADED.to_owned(),
        });
    }
    Cow::Owned(Message::User(user))
}

/// 这一步的 ToolSearch 形式（docs/specs/rust-cli/tools-more.md §3.2，工作线 F89）：开不开见 [`tool_search_enabled`]，开着时
/// 按端点的能力（`models::tool_search_mode`，产品路径一律本地）。主循环与子代理发请求、`/context` 的计数共用这一个判定；
/// 发请求时另要有延迟工具（[`wire_tools`]）。`prompt` 是这一步的工具清单与模式（描述按它生成）。
pub fn step_tool_search(
    tools: &[Arc<dyn Tool>],
    prompt: &PromptContext,
    (setting, model, capabilities): (ToolSearchSetting, &str, Option<&ModelCapabilities>),
) -> ToolSearchMode {
    let window = models::context_window(model, capabilities.map(|found| found.context_window));
    if tool_search_enabled(tools, prompt, (setting, window)) {
        models::tool_search_mode(capabilities.map(|found| found.supports_tool_search))
    } else {
        ToolSearchMode::Off
    }
}

/// 这一步 ToolSearch 开不开（§3.2.1 的第 2 层）：工具里有 ToolSearch（关时它不启用，deny 时不在），
/// `ENABLE_TOOL_SEARCH=auto[:N]` 时这一步的延迟工具到了门槛。`window` 是模型的上下文窗口（`models::context_window`）。
fn tool_search_enabled(
    tools: &[Arc<dyn Tool>],
    prompt: &PromptContext,
    (setting, window): (ToolSearchSetting, u64),
) -> bool {
    tools.iter().any(|tool| tool.name() == TOOL_SEARCH)
        && setting.reached(window, || deferred_chars(tools, prompt))
}

/// 延迟工具的大小（自动门槛用）：每个延迟工具的名字、描述与 `input_schema` 紧凑 JSON 的 UTF-16 长度之和。
fn deferred_chars(tools: &[Arc<dyn Tool>], prompt: &PromptContext) -> u64 {
    let length = |text: &str| text.encode_utf16().count() as u64;
    tools
        .iter()
        .filter(|tool| is_deferred(tool.as_ref()))
        .map(|tool| {
            length(tool.name())
                + length(&tool.description(prompt))
                + length(&tool.input_schema().to_string())
        })
        .sum()
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

/// `mode`：这一步的 ToolSearch 形式（[`step_tool_search`]）。`history` 是这一步发给模型的活动历史。
pub(crate) fn wire_tools<'a>(
    tools: &[Arc<dyn Tool>],
    history: impl IntoIterator<Item = &'a Message>,
    prompt: &PromptContext,
    (mode, order): (ToolSearchMode, ToolOrder),
) -> WireTools {
    let mut sorted: Vec<&Arc<dyn Tool>> = tools.iter().collect();
    if order == ToolOrder::ByName {
        sorted.sort_by(|left, right| {
            left.is_mcp()
                .cmp(&right.is_mcp())
                .then_with(|| compare_names(left.name(), right.name()))
        });
    }
    let active = mode.is_on()
        && sorted.iter().any(|tool| tool.name() == TOOL_SEARCH)
        && sorted.iter().any(|tool| is_deferred(tool.as_ref()));
    let mode = if active { mode } else { ToolSearchMode::Off };
    // 本地 ToolSearch（工作线 F84，tools-more.md §3.2.2）：已加载的工具以普通定义发出，不带 `defer_loading`，也没有
    // 占位工具——端点不认这两样（F52 的起因：网关后面的 Bedrock 拒绝随之生成的 `advanced-tool-use` beta）。
    let native = mode == ToolSearchMode::Native;
    // 已加载 = 引用过的 + 活动历史开头那条压缩边界带下来的（docs/specs/rust-cli/compact.md §3.5）。
    // 修 bug（F82）：以前只看引用，压缩之后已加载的延迟工具从 tools 里消失。
    let carried = if active {
        loaded(history)
    } else {
        Default::default()
    };
    let mut definitions = Vec::new();
    let mut flags = BTreeMap::new();
    for tool in sorted {
        let deferred = active && is_deferred(tool.as_ref());
        let keep = if active {
            !deferred || carried.contains(tool.name())
        } else {
            tool.name() != TOOL_SEARCH
        };
        if !keep {
            continue;
        }
        if deferred && native {
            flags.insert(tool.name().to_owned(), deferred_flag());
        }
        definitions.push(ToolDefinition {
            name: tool.name().to_owned(),
            description: tool.description(prompt),
            input_schema: tool.input_schema(),
            cache_control: None,
        });
    }
    // 占位工具（缺省开，只在原生时）：插在倒数第二位。
    if native && !definitions.iter().any(|tool| tool.name == PLACEHOLDER) {
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
        mode,
    }
}

#[cfg(test)]
#[path = "deferred_tests.rs"]
mod tests;
