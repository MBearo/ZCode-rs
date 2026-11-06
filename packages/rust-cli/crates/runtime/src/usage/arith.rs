//! `/context` 的类别与算术（docs/specs/rust-cli/context-usage.md §4）：输入是已经计好的数，
//! 不做 IO、不发请求。

use rust_cli_kernel_api::context_usage::{
    AgentUsage, CategoryKind, ContextUsage, McpToolUsage, MemoryFileUsage, PromptSectionUsage,
    SkillUsage, ToolUsage, UsageCategory, WindowSource,
};
use rust_cli_kernel_api::tool_names::SKILL;

use super::counts::TOOL_OVERHEAD;
use super::detail::{self, Piece};

/// 百分比的满值。
const PERCENT: u64 = rust_cli_base::units::PERCENT;

/// 一个 MCP 工具：名字、定义的估算（分摊用）、是否已加载（延迟加载开启时没加载的算延迟类别）。
pub(crate) struct McpTool {
    pub(crate) name: String,
    pub(crate) estimate: u64,
    pub(crate) loaded: bool,
}

/// 消息的来源（§4 第 4 条）。
pub(crate) enum Messages {
    /// 没有用量锚点：活动历史的本地估算（工作线 F93 之前是计数接口给的数）。
    Counted(u64),
    /// 最近一次响应的输入（含缓存读写）与它之后消息的估算。
    Measured { input: u64, tail: u64 },
}

/// 已经计好的输入。
pub(crate) struct Measured {
    pub(crate) model: String,
    pub(crate) window: u64,
    pub(crate) source: WindowSource,
    pub(crate) auto: bool,
    pub(crate) buffer: Option<(&'static str, u64)>,
    /// system prompt 的每段（名字与估算）。
    pub(crate) sections: Vec<PromptSectionUsage>,
    pub(crate) memory_files: Vec<MemoryFileUsage>,
    /// 列出的内置工具一次计数的原值。
    pub(crate) listed_tools: u64,
    /// 列出的内置工具各自的估算（已减去开销）：`System tools` 明细的权重（§3.1）。
    pub(crate) listed: Vec<ToolUsage>,
    /// 每个延迟的内置工具（已减去开销）。
    pub(crate) deferred_tools: Vec<ToolUsage>,
    /// MCP 工具一次计数的原值。
    pub(crate) mcp_total: u64,
    pub(crate) mcp_tools: Vec<McpTool>,
    /// 延迟加载开启（决定 MCP 工具是否分出延迟类别）。
    pub(crate) tool_search: bool,
    pub(crate) mcp_instructions: u64,
    pub(crate) agents: Vec<AgentUsage>,
    pub(crate) skills: Vec<SkillUsage>,
    pub(crate) messages: Messages,
    /// 消息的内容块（种类与权重，§3.1）。
    pub(crate) pieces: Vec<Piece>,
}

/// JS `Math.round(part / whole * scale)`（非负数）。
fn round_ratio(part: u64, whole: u64, scale: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    let (part, whole, scale) = (u128::from(part), u128::from(whole), u128::from(scale));
    u64::try_from((part * scale * 2 + whole) / (whole * 2)).unwrap_or(u64::MAX)
}

/// 列出的内置工具的明细（§3.1）：按各自的估算分摊 `System tools`；skill 清单在 Skill 的描述里、已算在 `Skills`，
/// Skill 的权重先减去它。0 的不列。
fn system_tools(listed: &[ToolUsage], category: u64, skills: u64) -> Vec<ToolUsage> {
    let weights: Vec<u64> = listed
        .iter()
        .map(|tool| {
            if tool.name == SKILL {
                tool.tokens.saturating_sub(skills)
            } else {
                tool.tokens
            }
        })
        .collect();
    listed
        .iter()
        .zip(detail::apportion(category, &weights))
        .filter(|(_, tokens)| *tokens > 0)
        .map(|(tool, tokens)| ToolUsage {
            name: tool.name.clone(),
            tokens,
        })
        .collect()
}

/// MCP 工具的明细与（已加载，延迟）两个类别的数：总数减开销后按估算比例分摊。
fn mcp(measured: &Measured) -> (Vec<McpToolUsage>, u64, u64) {
    let budget = measured.mcp_total.saturating_sub(TOOL_OVERHEAD);
    let estimates: u64 = measured.mcp_tools.iter().map(|tool| tool.estimate).sum();
    let mut loaded = 0;
    let mut deferred = 0;
    let details = measured
        .mcp_tools
        .iter()
        .map(|tool| {
            let tokens = round_ratio(tool.estimate, estimates.max(1), budget);
            if tool.loaded {
                loaded += tokens;
            } else {
                deferred += tokens;
            }
            McpToolUsage {
                name: tool.name.clone(),
                server: rust_cli_kernel_api::mcp_name::parse(&tool.name)
                    .map_or("unknown", |parsed| parsed.server)
                    .to_owned(),
                tokens,
                deferred: !tool.loaded,
            }
        })
        .collect();
    if !measured.tool_search {
        // 没有延迟加载：全部算 MCP tools。
        let total = if measured.mcp_tools.is_empty() {
            0
        } else {
            budget
        };
        return (details, total, 0);
    }
    (details, loaded, deferred)
}

pub(crate) fn usage(measured: Measured) -> ContextUsage {
    let mut categories = Vec::new();
    let mut push = |name: &str, tokens: u64, kind: CategoryKind| {
        if tokens > 0 {
            categories.push(UsageCategory {
                name: name.to_owned(),
                tokens,
                kind,
            });
        }
    };
    let skills: u64 = measured.skills.iter().map(|skill| skill.tokens).sum();
    let (mcp_tools, mcp_loaded, mcp_deferred) = mcp(&measured);
    push(
        "System prompt",
        measured.sections.iter().map(|section| section.tokens).sum(),
        CategoryKind::Used,
    );
    let listed = measured.listed_tools.saturating_sub(skills);
    push("System tools", listed, CategoryKind::Used);
    push("MCP tools", mcp_loaded, CategoryKind::Used);
    push(
        "MCP server instructions",
        measured.mcp_instructions,
        CategoryKind::Used,
    );
    push("MCP tools (deferred)", mcp_deferred, CategoryKind::Deferred);
    push(
        "System tools (deferred)",
        measured.deferred_tools.iter().map(|tool| tool.tokens).sum(),
        CategoryKind::Deferred,
    );
    push(
        "Custom agents",
        measured.agents.iter().map(|agent| agent.tokens).sum(),
        CategoryKind::Used,
    );
    push(
        "Memory files",
        measured.memory_files.iter().map(|file| file.tokens).sum(),
        CategoryKind::Used,
    );
    push("Skills", skills, CategoryKind::Used);
    let used = |categories: &[UsageCategory]| -> u64 {
        categories
            .iter()
            .filter(|category| category.kind != CategoryKind::Deferred)
            .map(|category| category.tokens)
            .sum()
    };
    let buffer = measured.buffer.map_or(0, |(_, tokens)| tokens);
    let before = used(&categories);
    let (messages, total) = match measured.messages {
        Messages::Counted(tokens) => (tokens, None),
        Messages::Measured { input, tail } => {
            let measured_total = input.saturating_sub(before) + tail;
            let room = measured
                .window
                .saturating_sub(before)
                .saturating_sub(buffer);
            (measured_total.min(room), Some(before + measured_total))
        }
    };
    if messages > 0 {
        categories.push(UsageCategory {
            name: "Messages".to_owned(),
            tokens: messages,
            kind: CategoryKind::Used,
        });
    }
    let after = used(&categories);
    if let Some((name, tokens)) = measured.buffer {
        categories.push(UsageCategory {
            name: name.to_owned(),
            tokens,
            kind: CategoryKind::Buffer,
        });
    }
    categories.push(UsageCategory {
        name: "Free space".to_owned(),
        tokens: measured.window.saturating_sub(after).saturating_sub(buffer),
        kind: CategoryKind::Free,
    });
    let total = total.unwrap_or(after);
    let (messages, tool_results) = detail::messages(messages, &measured.pieces);
    ContextUsage {
        model: measured.model,
        total_tokens: total,
        raw_max_tokens: measured.window,
        percentage: round_ratio(total, measured.window, PERCENT),
        window_source: measured.source,
        auto_compact: measured.auto,
        categories,
        mcp_tools,
        memory_files: measured.memory_files,
        agents: measured.agents,
        skills: measured.skills,
        system_tools: system_tools(&measured.listed, listed, skills),
        deferred_tools: measured
            .deferred_tools
            .into_iter()
            .filter(|tool| tool.tokens > 0)
            .collect(),
        prompt_sections: measured
            .sections
            .into_iter()
            .filter(|section| section.tokens > 0)
            .collect(),
        messages,
        tool_results,
    }
}

#[cfg(test)]
#[path = "arith_tests.rs"]
mod tests;
