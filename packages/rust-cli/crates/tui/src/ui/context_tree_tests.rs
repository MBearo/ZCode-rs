//! 上下文的页的大纲（tui-panels.md §4.3）：顶层是卡的五个组（次序同图例），下面是内核的明细；开合与选中按路径走。
//! [`sample`] 是页的测试共用的会话（数照设计稿 `design/tui-context-page/src/data.mjs` 的样子，每个列表的和等于它的类别）。
use rust_cli_kernel_api::context_usage::{
    AgentUsage, CategoryKind, ContextUsage, McpToolUsage, MemoryFileUsage, MessageUsage,
    PromptSectionUsage, SkillUsage, ToolResultUsage, ToolUsage, UsageCategory, WindowSource,
};

use super::*;

fn cat(name: &str, tokens: u64, kind: CategoryKind) -> UsageCategory {
    UsageCategory {
        name: name.into(),
        tokens,
        kind,
    }
}

fn tool(name: &str, tokens: u64) -> ToolUsage {
    ToolUsage {
        name: name.into(),
        tokens,
    }
}

fn mcp(name: &str, server: &str, tokens: u64, deferred: bool) -> McpToolUsage {
    McpToolUsage {
        name: name.into(),
        server: server.into(),
        tokens,
        deferred,
    }
}

/// 用掉 125,690（62 %）、窗口 200k、缓冲 40k 的会话；`messages` 是 Messages 一类的 token（明细按比例跟着）。
pub(crate) fn sample() -> ContextUsage {
    let used = 6_730 + 14_200 + 9_990 + 820 + 630 + 3_120 + 1_200 + 89_000;
    let window = 200_000;
    let buffer = 40_000;
    ContextUsage {
        model: "scripted".into(),
        total_tokens: used,
        raw_max_tokens: window,
        percentage: used * 100 / window,
        window_source: WindowSource::ModelDefault,
        auto_compact: true,
        categories: vec![
            cat("System prompt", 6_730, CategoryKind::Used),
            cat("System tools", 14_200, CategoryKind::Used),
            cat("MCP tools", 9_990, CategoryKind::Used),
            cat("MCP server instructions", 820, CategoryKind::Used),
            cat("MCP tools (deferred)", 2_000, CategoryKind::Deferred),
            cat("System tools (deferred)", 5_300, CategoryKind::Deferred),
            cat("Custom agents", 630, CategoryKind::Used),
            cat("Memory files", 3_120, CategoryKind::Used),
            cat("Skills", 1_200, CategoryKind::Used),
            cat("Messages", 89_000, CategoryKind::Used),
            cat("Autocompact buffer", buffer, CategoryKind::Buffer),
            cat("Free space", window - used - buffer, CategoryKind::Free),
        ],
        mcp_tools: vec![
            mcp("mcp__github__create_pull_request", "github", 6_000, false),
            mcp("mcp__github__search_code", "github", 2_000, false),
            mcp("mcp__docs__search", "docs", 1_990, false),
            mcp("mcp__docs__read_page", "docs", 2_000, true),
        ],
        memory_files: vec![
            MemoryFileUsage {
                path: "AGENTS.md".into(),
                kind: "Project".into(),
                tokens: 2_240,
            },
            MemoryFileUsage {
                path: "~/.zcode/AGENTS.md".into(),
                kind: "User".into(),
                tokens: 880,
            },
        ],
        agents: vec![
            AgentUsage {
                agent_type: "reviewer".into(),
                source: "Project".into(),
                tokens: 410,
            },
            AgentUsage {
                agent_type: "planner".into(),
                source: "User".into(),
                tokens: 220,
            },
        ],
        skills: vec![
            SkillUsage {
                name: "review".into(),
                source: "Project".into(),
                plugin: None,
                tokens: 700,
            },
            SkillUsage {
                name: "deploy".into(),
                source: "User".into(),
                plugin: Some("ops".into()),
                tokens: 500,
            },
        ],
        prompt_sections: [
            ("Core instructions", 3_900),
            ("Communication", 610),
            ("Caution", 540),
            ("Memory", 380),
            ("Session guidance", 1_300),
        ]
        .into_iter()
        .map(|(name, tokens)| PromptSectionUsage {
            name: name.into(),
            tokens,
        })
        .collect(),
        system_tools: vec![
            tool("Bash", 2_930),
            tool("Agent", 2_410),
            tool("Read", 1_310),
            tool("Edit", 1_580),
            tool("Grep", 1_120),
            tool("Write", 4_850),
        ],
        deferred_tools: vec![tool("NotebookEdit", 3_000), tool("WebSearch", 2_300)],
        messages: [
            ("Your messages", 4_100),
            ("Replies", 9_800),
            ("Tool calls", 6_600),
            ("Tool results", 68_500),
        ]
        .into_iter()
        .map(|(kind, tokens)| MessageUsage {
            kind: kind.into(),
            tokens,
        })
        .collect(),
        tool_results: [
            ("Read", 23, 41_200),
            ("Bash", 31, 17_900),
            ("Grep", 12, 6_000),
            ("WebFetch", 2, 3_400),
        ]
        .into_iter()
        .map(|(tool, calls, tokens)| ToolResultUsage {
            tool: tool.into(),
            calls,
            tokens,
        })
        .collect(),
    }
}

fn path(parts: &[&str]) -> Path {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

fn find<'a>(nodes: &'a [Node], parts: &[&str]) -> &'a Node {
    let mut level = nodes;
    let mut node = None;
    for part in parts {
        node = level.iter().find(|n| n.key == *part);
        level = &node.unwrap_or_else(|| panic!("{part} in {parts:?}")).kids;
    }
    node.expect("a path")
}

/// 顶层是卡的五个组：次序与档同卡的图例（大的在前），数是组里用掉的类别之和；延迟的类别不算进组。
#[test]
fn the_top_rows_are_the_card_groups_largest_first() {
    let nodes = outline(&sample(), &Places::default());
    let top: Vec<_> = nodes
        .iter()
        .map(|n| (n.name.as_str(), n.tokens, n.rank))
        .collect();
    assert_eq!(
        top,
        vec![
            ("Messages", 89_000, Some(0)),
            ("Tools", 14_830, Some(1)),
            ("MCP", 10_810, Some(2)),
            ("System", 9_850, Some(3)),
            ("Skills", 1_200, Some(4)),
        ]
    );
    let notes: Vec<_> = nodes.iter().map(|n| n.note.as_str()).collect();
    assert_eq!(
        notes,
        vec![
            "4 kinds",
            "6 tools · 2 agents",
            "2 servers",
            "prompt · 2 files",
            "2 skills"
        ]
    );
    let pointers: Vec<_> = nodes.iter().map(|n| n.pointer).collect();
    assert_eq!(
        pointers,
        vec![None, None, Some("/mcp"), None, Some("/skills")]
    );
}

/// 每一层大的在前，按需加载的行在最后（不算进合计）；明细的和等于它的那一行。
#[test]
fn each_level_is_largest_first_with_quiet_rows_last() {
    let nodes = outline(&sample(), &Places::default());
    let messages = find(&nodes, &["messages"]);
    let kinds: Vec<_> = messages.kids.iter().map(|n| n.name.as_str()).collect();
    assert_eq!(
        kinds,
        ["Tool results", "Replies", "Tool calls", "Your messages"]
    );
    let results = find(&nodes, &["messages", "Tool results"]);
    assert_eq!(results.note, "68 calls");
    assert_eq!(results.kids[0].note, "23 calls");
    assert_eq!(
        results.kids.iter().map(|n| n.tokens).sum::<u64>(),
        results.tokens
    );
    let system_tools = find(&nodes, &["tools", "System tools"]);
    let last = system_tools.kids.last().expect("a row");
    assert_eq!(
        (
            last.name.as_str(),
            last.note.as_str(),
            last.tokens,
            last.quiet
        ),
        ("2 more on demand", "not loaded", 5_300, true)
    );
    assert_eq!(system_tools.kids[0].name, "Write");
    // MCP：服务器的工具去掉 `mcp__服务器__`，延迟的一行与服务器的说明在后面。
    let servers: Vec<_> = find(&nodes, &["MCP"])
        .kids
        .iter()
        .map(|n| (n.name.as_str(), n.tokens))
        .collect();
    assert_eq!(
        servers,
        vec![
            ("github", 8_000),
            ("docs", 1_990),
            ("Server instructions", 820),
            ("1 more on demand", 2_000),
        ]
    );
    assert_eq!(
        find(&nodes, &["MCP", "github"]).kids[0].name,
        "create_pull_request"
    );
    let memory = find(&nodes, &["system", "Memory files"]);
    assert_eq!(
        (memory.pointer, memory.kids[1].note.as_str()),
        (Some("/memory"), "user")
    );
    assert_eq!(find(&nodes, &["skills", "deploy"]).note, "user (ops)");
}

/// 规则文件：工作区里的写相对路径，家目录写成 `~`，别处原样；路径里这一段仍是完整路径。
#[test]
fn memory_files_show_short_paths() {
    let mut usage = sample();
    usage.memory_files[0].path = "/Users/dev/project/AGENTS.md".into();
    usage.memory_files[1].path = "/Users/dev/.zcode/AGENTS.md".into();
    let places = Places {
        root: "/Users/dev/project",
        home: Some("/Users/dev"),
    };
    let nodes = outline(&usage, &places);
    let memory = find(&nodes, &["system", "Memory files"]);
    let shown: Vec<_> = memory
        .kids
        .iter()
        .map(|n| (n.name.as_str(), n.key.as_str()))
        .collect();
    assert_eq!(
        shown,
        vec![
            ("AGENTS.md", "/Users/dev/project/AGENTS.md"),
            ("~/.zcode/AGENTS.md", "/Users/dev/.zcode/AGENTS.md"),
        ]
    );
    assert_eq!(Places::default().show("/etc/AGENTS.md"), "/etc/AGENTS.md");
}

/// 旧内核（没有明细的列表）：那几行没有子行、不能打开。
#[test]
fn rows_without_detail_cannot_open() {
    let mut usage = sample();
    usage.messages.clear();
    usage.tool_results.clear();
    usage.prompt_sections.clear();
    usage.system_tools.clear();
    let nodes = outline(&usage, &Places::default());
    assert!(find(&nodes, &["messages"]).kids.is_empty());
    assert_eq!(find(&nodes, &["messages"]).note, "");
    assert!(find(&nodes, &["system", "System prompt"]).kids.is_empty());
    assert!(find(&nodes, &["tools", "System tools"]).kids.is_empty());
    assert_eq!(find(&nodes, &["tools"]).note, "2 agents");
}

/// 按键：↓ 移动，⏎/→ 打开，← 在开着的行上合上、在合着的行或叶子上去上一层；合上时下面开着的一起忘掉。
#[test]
fn keys_open_close_and_walk_by_path() {
    let nodes = outline(&sample(), &Places::default());
    let mut open = BTreeSet::new();
    let mut chosen = Path::new();
    step(&nodes, &mut open, &mut chosen, Move::Toggle);
    assert_eq!((chosen.clone(), open.len()), (path(&["messages"]), 1));
    step(&nodes, &mut open, &mut chosen, Move::By(1));
    assert_eq!(chosen, path(&["messages", "Tool results"]));
    step(&nodes, &mut open, &mut chosen, Move::Open);
    step(&nodes, &mut open, &mut chosen, Move::By(1));
    assert_eq!(chosen, path(&["messages", "Tool results", "Read"]));
    // 叶子上 ← 去上一层，再 ← 合上它，再 ← 去顶层。
    step(&nodes, &mut open, &mut chosen, Move::Close);
    assert_eq!(chosen, path(&["messages", "Tool results"]));
    step(&nodes, &mut open, &mut chosen, Move::Close);
    assert!(!open.contains(&path(&["messages", "Tool results"])));
    step(&nodes, &mut open, &mut chosen, Move::Close);
    assert_eq!(chosen, path(&["messages"]));
    // 再打开 Tool results，然后在顶层合上：下面开着的一起忘掉。
    step(&nodes, &mut open, &mut chosen, Move::By(1));
    step(&nodes, &mut open, &mut chosen, Move::Open);
    step(&nodes, &mut open, &mut chosen, Move::By(-1));
    step(&nodes, &mut open, &mut chosen, Move::Toggle);
    assert!(open.is_empty());
    step(&nodes, &mut open, &mut chosen, Move::Last);
    assert_eq!(chosen, path(&["skills"]));
    step(&nodes, &mut open, &mut chosen, Move::By(10));
    assert_eq!(chosen, path(&["skills"]));
    step(&nodes, &mut open, &mut chosen, Move::First);
    assert_eq!(chosen, path(&["messages"]));
}

/// 选中的路径不在了（刷新后那一行没了、或上一层合上了）：选它还在的最近的上一层，都不在时第一行。
#[test]
fn a_missing_choice_falls_back_to_its_nearest_ancestor() {
    let nodes = outline(&sample(), &Places::default());
    let open = BTreeSet::from([path(&["messages"])]);
    let rows = visible(&nodes, &open);
    let gone = path(&["messages", "Tool results", "Read"]);
    assert_eq!(
        rows[chosen_at(&rows, &gone)].0,
        path(&["messages", "Tool results"])
    );
    assert_eq!(chosen_at(&rows, &path(&["nothing"])), 0);
}
