//! `/context` 页面的明细与建议（tui-panels.md §4.2）：折叠、展开、建议的条件与排序。
use rust_cli_kernel_api::context_usage::{
    AgentUsage, CategoryKind, ContextUsage, McpToolUsage, MemoryFileUsage, SkillUsage,
    UsageCategory, WindowSource,
};

use super::*;
use crate::ui::palette::{ColorLevel, Palette};

fn usage(total: u64, memory: &[(&str, u64)]) -> ContextUsage {
    let window = 200_000;
    ContextUsage {
        model: "scripted".into(),
        total_tokens: total,
        raw_max_tokens: window,
        percentage: total * 100 / window,
        window_source: WindowSource::ModelDefault,
        auto_compact: true,
        categories: vec![
            UsageCategory {
                name: "Messages".into(),
                tokens: total,
                kind: CategoryKind::Used,
            },
            UsageCategory {
                name: "Free space".into(),
                tokens: window - total,
                kind: CategoryKind::Free,
            },
        ],
        mcp_tools: vec![McpToolUsage {
            name: "mcp__docs__search".into(),
            server: "docs".into(),
            tokens: 1_200,
        }],
        memory_files: memory
            .iter()
            .map(|(path, tokens)| MemoryFileUsage {
                path: (*path).into(),
                kind: "Project".into(),
                tokens: *tokens,
            })
            .collect(),
        agents: Vec::new(),
        skills: vec![
            SkillUsage {
                name: "review".into(),
                source: "project".into(),
                plugin: None,
                tokens: 300,
            },
            SkillUsage {
                name: "deploy".into(),
                source: "user".into(),
                plugin: None,
                tokens: 200,
            },
        ],
    }
}

fn text(usage: &ContextUsage, all: bool, lang: Lang) -> Vec<String> {
    let palette = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
    let strings = crate::ui::i18n::strings(lang);
    let ctx = Ctx {
        palette: &palette,
        lang,
        strings,
        width: 100,
        expanded: true,
        elapsed: std::time::Duration::ZERO,
        now_ms: 0,
        workspace: "/Users/dev/project",
        workspace_label: "~/project",
        version: "0.1.0",
        markdown: None,
    };
    page_lines(usage, all, &ctx)
        .iter()
        .map(|line| {
            let text = crate::ui::text::plain(line);
            text.strip_prefix("  ").unwrap_or(&text).to_owned()
        })
        .collect()
}

#[test]
fn details_fold_into_counts_until_all_expands_them() {
    let usage = usage(10_000, &[("AGENTS.md", 800)]);
    let folded = text(&usage, false, Lang::En);
    let at = |lines: &[String], line: &str| lines.iter().position(|l| l == line);
    let mcp = at(&folded, "MCP tools · /mcp").unwrap();
    assert_eq!(folded[mcp + 1], "└ 1 tool · 1.2k tokens");
    let skills = at(&folded, "Skills · /skills").unwrap();
    assert_eq!(folded[skills + 1], "└ 2 skills · 500 tokens");
    assert_eq!(folded.last().unwrap(), "/context all to expand");
    // 自定义 agent 为空：这一段不显示。
    assert!(at(&folded, "Custom agents").is_none());

    let open = text(&usage, true, Lang::En);
    let skills = at(&open, "Skills · /skills").unwrap();
    // skill 的数是估算（`~`）。
    assert_eq!(open[skills + 1], "├ review (project): ~300 tokens");
    assert_eq!(open[skills + 2], "└ deploy (user): ~200 tokens");
    assert!(!open.iter().any(|l| l.contains("to expand")));
    let memory = at(&open, "Memory files · /memory").unwrap();
    assert_eq!(open[memory + 1], "└ Project (AGENTS.md): 800 tokens");
}

#[test]
fn suggestions_cover_a_full_window_and_large_memory_files() {
    // 80% 以上：警告在前；规则文件 ≥ 5% 且 ≥ 5k：带节省量与最大的三个。
    let usage = usage(
        170_000,
        &[
            ("a.md", 6_000),
            ("b.md", 3_000),
            ("c.md", 1_000),
            ("d.md", 1_000),
        ],
    );
    let lines = text(&usage, false, Lang::En);
    let at = lines.iter().position(|l| l == "Suggestions").unwrap();
    assert_eq!(lines[at + 1], "⚠ Context is 85% full");
    assert!(
        lines[at + 2].starts_with("  Autocompact will trigger soon"),
        "{lines:?}"
    );
    // 条目之间空一行；说明按宽度换行，续行也缩进 2 列。
    let memory = lines
        .iter()
        .position(|l| l == "ℹ Memory files using 11k tokens (6%) → save ~3.3k")
        .unwrap();
    assert!(memory > at + 3, "{lines:?}");
    assert_eq!(lines[memory - 1], "");
    assert!(lines[memory - 2].starts_with("  "), "{lines:?}");
    assert_eq!(
        lines[memory + 1],
        "  Largest: a.md (6k), b.md (3k), c.md (1k). Review and prune stale entries."
    );
    // 都不满足时没有建议一节。
    let calm = usage_without_memory();
    assert!(
        !text(&calm, false, Lang::En)
            .iter()
            .any(|l| l == "Suggestions")
    );
    // 中文。
    let zh = text(&usage, false, Lang::Zh);
    assert!(zh.iter().any(|l| l == "建议"), "{zh:?}");
    assert!(zh.iter().any(|l| l == "⚠ 上下文已用 85%"), "{zh:?}");
    assert!(zh.iter().any(|l| l == "/context all 展开明细"), "{zh:?}");
}

fn usage_without_memory() -> ContextUsage {
    usage(10_000, &[])
}

/// `/agents` 命令已删（L33）：标题只留 `Custom agents`，不再指向它（以前是原文 `Custom agents · /agents`）。
#[test]
fn the_custom_agents_title_names_no_command() {
    let mut usage = usage(10_000, &[]);
    usage.agents = vec![AgentUsage {
        agent_type: "reviewer".into(),
        source: "project".into(),
        tokens: 400,
    }];
    let lines = text(&usage, false, Lang::En);
    let title = lines
        .iter()
        .position(|line| line == "Custom agents")
        .unwrap();
    assert_eq!(lines[title + 1], "└ 1 agent · 400 tokens");
    assert!(!lines.iter().any(|line| line.contains("/agents")));
}
