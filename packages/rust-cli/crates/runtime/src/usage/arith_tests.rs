//! 类别与算术（context-usage.md §4）：用一次真实运行的数字（fixture 回的计数）核对。
use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::context_usage::{CategoryKind, MessageUsage};

use super::*;
use crate::usage::detail::Kind;

fn tools(tokens: &[u64]) -> Vec<ToolUsage> {
    tokens
        .iter()
        .enumerate()
        .map(|(index, tokens)| ToolUsage {
            name: format!("T{index}"),
            tokens: *tokens,
        })
        .collect()
}

fn c1() -> Measured {
    Measured {
        model: GLM_5_3.into(),
        window: 1_000_000,
        source: WindowSource::ModelDefault,
        auto: true,
        buffer: Some(("Autocompact buffer", 33_000)),
        sections: [325, 44, 110, 150, 61, 577, 166, 91, 90, 33]
            .into_iter()
            .enumerate()
            .map(|(index, tokens)| PromptSectionUsage {
                name: format!("S{index}"),
                tokens,
            })
            .collect(),
        memory_files: Vec::new(),
        listed_tools: 6689,
        // 列出的工具各自减去 500 之后（明细的权重）；最后一个是 Skill（含 skill 清单）。
        listed: [("Bash", 1200), ("Read", 600), ("Edit", 600), (SKILL, 2300)]
            .into_iter()
            .map(|(name, tokens)| ToolUsage {
                name: name.into(),
                tokens,
            })
            .collect(),
        // 10 个延迟工具，各自减去 500 之后。
        deferred_tools: tools(&[528, 0, 0, 524, 147, 0, 940, 0, 0, 0]),
        mcp_total: 21,
        mcp_tools: Vec::new(),
        tool_search: true,
        mcp_instructions: 0,
        agents: Vec::new(),
        skills: [482, 236, 83, 214, 63, 64, 115, 361, 84, 123, 83, 22, 30]
            .into_iter()
            .map(|tokens| SkillUsage {
                name: "skill".into(),
                source: "Built-in".into(),
                plugin: None,
                tokens,
            })
            .collect(),
        messages: Messages::Counted(21),
        pieces: vec![
            Piece {
                kind: Kind::Yours,
                tool: None,
                weight: 7,
            },
            Piece {
                kind: Kind::Replies,
                tool: None,
                weight: 14,
            },
        ],
    }
}

fn names(usage: &ContextUsage) -> Vec<(&str, u64, CategoryKind)> {
    usage
        .categories
        .iter()
        .map(|category| (category.name.as_str(), category.tokens, category.kind))
        .collect()
}

#[test]
fn the_c1_numbers_add_up_by_category() {
    let usage = usage(c1());
    assert_eq!(
        names(&usage),
        [
            ("System prompt", 1647, CategoryKind::Used),
            ("System tools", 4729, CategoryKind::Used),
            ("System tools (deferred)", 2139, CategoryKind::Deferred),
            ("Skills", 1960, CategoryKind::Used),
            ("Messages", 21, CategoryKind::Used),
            ("Autocompact buffer", 33_000, CategoryKind::Buffer),
            ("Free space", 958_643, CategoryKind::Free),
        ]
    );
    assert_eq!(usage.total_tokens, 8357);
    assert_eq!(usage.percentage, 1);
    assert_eq!(usage.over_limit(), None);
}

/// 有用量锚点：消息 = 锚点的输入 − 其他类别 + 之后的估算，受窗口与缓冲限制；总量不受限制。
#[test]
fn an_anchor_measures_the_messages_from_the_last_usage() {
    let mut measured = c1();
    measured.messages = Messages::Measured {
        input: 20_000,
        tail: 300,
    };
    let usage = usage(measured);
    let messages = usage
        .categories
        .iter()
        .find(|category| category.name == "Messages")
        .unwrap();
    assert_eq!(messages.tokens, 20_000 - 8336 + 300);
    assert_eq!(usage.total_tokens, 20_300);
    assert_eq!(usage.percentage, 2);

    let mut full = c1();
    full.window = 200_000;
    full.source = WindowSource::Auto;
    full.buffer = None;
    full.messages = Messages::Measured {
        input: 230_000,
        tail: 0,
    };
    let usage = super::usage(full);
    let messages = usage
        .categories
        .iter()
        .find(|category| category.name == "Messages")
        .unwrap();
    assert_eq!(messages.tokens, 200_000 - 8336);
    assert_eq!(usage.total_tokens, 230_000);
    assert_eq!(usage.percentage, 115);
    assert!(
        usage
            .over_limit()
            .unwrap()
            .starts_with("Context exceeds the 200k-token limit by 30k tokens")
    );
    let free = usage.categories.last().unwrap();
    assert_eq!((free.name.as_str(), free.tokens), ("Free space", 0));
}

/// MCP 工具：总数减开销，按估算比例分摊；延迟加载时没加载的进延迟类别。
#[test]
fn mcp_tools_share_one_count() {
    let mut measured = c1();
    measured.mcp_total = 1500;
    measured.mcp_tools = vec![
        McpTool {
            name: "mcp__fixture__echo".into(),
            estimate: 300,
            loaded: true,
        },
        McpTool {
            name: "mcp__fixture__sum".into(),
            estimate: 100,
            loaded: false,
        },
    ];
    let usage = usage(measured);
    assert_eq!(usage.mcp_tools[0].server, "fixture");
    assert_eq!(usage.mcp_tools[0].tokens, 750);
    assert_eq!(usage.mcp_tools[1].tokens, 250);
    let found = |name: &str| {
        usage
            .categories
            .iter()
            .find(|category| category.name == name)
            .map(|category| category.tokens)
    };
    assert_eq!(found("MCP tools"), Some(750));
    assert_eq!(found("MCP tools (deferred)"), Some(250));
}

#[test]
fn without_tool_search_all_mcp_tools_count_and_compact_buffer_shows_when_auto_is_off() {
    let mut measured = c1();
    measured.tool_search = false;
    measured.auto = false;
    measured.buffer = Some(("Compact buffer", 3000));
    measured.mcp_total = 900;
    measured.mcp_tools = vec![McpTool {
        name: "mcp__x__y".into(),
        estimate: 10,
        loaded: false,
    }];
    let usage = usage(measured);
    let found = |name: &str| {
        usage
            .categories
            .iter()
            .find(|category| category.name == name)
            .map(|category| category.tokens)
    };
    assert_eq!(found("MCP tools"), Some(400));
    assert_eq!(found("MCP tools (deferred)"), None);
    assert_eq!(found("Compact buffer"), Some(3000));
}

/// 明细（context-usage.md §3.1）：每个列表的和等于它的类别，0 的项不列。
#[test]
fn every_detail_list_adds_up_to_its_category() {
    let usage = usage(c1());
    let category = |name: &str| {
        usage
            .categories
            .iter()
            .find(|category| category.name == name)
            .map_or(0, |category| category.tokens)
    };
    let sections: u64 = usage
        .prompt_sections
        .iter()
        .map(|section| section.tokens)
        .sum();
    assert_eq!(sections, category("System prompt"));
    assert_eq!(usage.prompt_sections[0].name, "S0");
    let listed: u64 = usage.system_tools.iter().map(|tool| tool.tokens).sum();
    assert_eq!(listed, category("System tools"));
    let deferred: u64 = usage.deferred_tools.iter().map(|tool| tool.tokens).sum();
    assert_eq!(deferred, category("System tools (deferred)"));
    assert_eq!(
        usage
            .deferred_tools
            .iter()
            .map(|tool| tool.name.as_str())
            .collect::<Vec<_>>(),
        ["T0", "T3", "T4", "T6"]
    );
    assert_eq!(
        usage.messages,
        [
            MessageUsage {
                kind: "Your messages".into(),
                tokens: 7
            },
            MessageUsage {
                kind: "Replies".into(),
                tokens: 14
            },
        ]
    );
}

/// Skill 的权重减去 skill 估算之和（1960），不重复算；其余按各自的估算分摊 `System tools`（4729）。
#[test]
fn the_skill_tool_weighs_without_the_skill_listing() {
    let usage = usage(c1());
    let tokens: Vec<(&str, u64)> = usage
        .system_tools
        .iter()
        .map(|tool| (tool.name.as_str(), tool.tokens))
        .collect();
    // 权重 1200、600、600、340（2300 − 1960），和 2740；4729 按比例：2071.09、1035.55、1035.55、586.81。
    // 整数部分和 4727，剩下的 2 给余数最大的 Skill 与 Read（Read 与 Edit 余数相同，按原来的顺序）。
    assert_eq!(
        tokens,
        [("Bash", 2071), ("Read", 1036), ("Edit", 1035), (SKILL, 587)]
    );

    // skill 比 Skill 工具还大时权重是 0，不列。
    let mut measured = c1();
    measured.listed[3].tokens = 100;
    let usage = super::usage(measured);
    assert!(usage.system_tools.iter().all(|tool| tool.name != SKILL));
}

/// 有锚点时 `Messages` 是测得的数，种类按估算的比例分它。
#[test]
fn an_anchor_scales_the_message_kinds_to_the_measured_total() {
    let mut measured = c1();
    measured.messages = Messages::Measured {
        input: 20_000,
        tail: 300,
    };
    let usage = usage(measured);
    // 20000 − 8336 + 300 = 11964，按 7 : 14 分：3988、7976。
    let kinds: Vec<(&str, u64)> = usage
        .messages
        .iter()
        .map(|kind| (kind.kind.as_str(), kind.tokens))
        .collect();
    assert_eq!(kinds, [("Your messages", 3988), ("Replies", 7976)]);
}
