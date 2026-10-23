//! `/context` 的类别与算术（docs/specs/rust-cli/context-usage.md §4）：输入是已经计好的数，
//! 不做 IO、不发请求。

use rust_cli_kernel_api::context_usage::{
    AgentUsage, CategoryKind, ContextUsage, McpToolUsage, MemoryFileUsage, SkillUsage,
    UsageCategory, WindowSource,
};

use super::counts::TOOL_OVERHEAD;

/// 一个 MCP 工具：名字、定义的估算（分摊用）、是否已加载（延迟加载开启时没加载的算延迟类别）。
pub(crate) struct McpTool {
    pub(crate) name: String,
    pub(crate) estimate: u64,
    pub(crate) loaded: bool,
}

/// 消息的来源（§4 第 4 条）。
pub(crate) enum Messages {
    /// 没有用量锚点：计数接口（或估算）给出的数。
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
    pub(crate) sections: Vec<u64>,
    pub(crate) memory_files: Vec<MemoryFileUsage>,
    /// 列出的内置工具一次计数的原值。
    pub(crate) listed_tools: u64,
    /// 每个延迟的内置工具（已减去开销）。
    pub(crate) deferred_tools: Vec<u64>,
    /// MCP 工具一次计数的原值。
    pub(crate) mcp_total: u64,
    pub(crate) mcp_tools: Vec<McpTool>,
    /// 延迟加载开启（决定 MCP 工具是否分出延迟类别）。
    pub(crate) tool_search: bool,
    pub(crate) mcp_instructions: u64,
    pub(crate) agents: Vec<AgentUsage>,
    pub(crate) skills: Vec<SkillUsage>,
    pub(crate) messages: Messages,
}

/// JS `Math.round(part / whole * scale)`（非负数）。
fn round_ratio(part: u64, whole: u64, scale: u64) -> u64 {
    if whole == 0 {
        return 0;
    }
    let (part, whole, scale) = (u128::from(part), u128::from(whole), u128::from(scale));
    u64::try_from((part * scale * 2 + whole) / (whole * 2)).unwrap_or(u64::MAX)
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
                server: tool.name.split("__").nth(1).unwrap_or("unknown").to_owned(),
                tokens,
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
        measured.sections.iter().sum(),
        CategoryKind::Used,
    );
    push(
        "System tools",
        measured.listed_tools.saturating_sub(skills),
        CategoryKind::Used,
    );
    push("MCP tools", mcp_loaded, CategoryKind::Used);
    push(
        "MCP server instructions",
        measured.mcp_instructions,
        CategoryKind::Used,
    );
    push("MCP tools (deferred)", mcp_deferred, CategoryKind::Deferred);
    push(
        "System tools (deferred)",
        measured.deferred_tools.iter().sum(),
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
    ContextUsage {
        model: measured.model,
        total_tokens: total,
        raw_max_tokens: measured.window,
        percentage: round_ratio(total, measured.window, 100),
        window_source: measured.source,
        auto_compact: measured.auto,
        categories,
        mcp_tools,
        memory_files: measured.memory_files,
        agents: measured.agents,
        skills: measured.skills,
    }
}

#[cfg(test)]
#[path = "arith_tests.rs"]
mod tests;
