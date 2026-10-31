//! `-p "/context"` 的 markdown（context-usage.md §5.1）：逐字核对整张表（skill
//! 只留本工程有的 `loop`）。
use rust_cli_base::models::GLM_5_3;
use rust_cli_kernel_api::context_usage::{
    CategoryKind, ContextUsage, MemoryFileUsage, SkillUsage, UsageCategory, WindowSource,
};

use super::*;

fn category(name: &str, tokens: u64, kind: CategoryKind) -> UsageCategory {
    UsageCategory {
        name: name.into(),
        tokens,
        kind,
    }
}

fn c1() -> ContextUsage {
    ContextUsage {
        model: GLM_5_3.into(),
        total_tokens: 8357,
        raw_max_tokens: 1_000_000,
        percentage: 1,
        window_source: WindowSource::ModelDefault,
        auto_compact: true,
        categories: vec![
            category("System prompt", 1647, CategoryKind::Used),
            category("System tools", 4729, CategoryKind::Used),
            category("System tools (deferred)", 2139, CategoryKind::Deferred),
            category("Skills", 1960, CategoryKind::Used),
            category("Messages", 21, CategoryKind::Used),
            category("Autocompact buffer", 33_000, CategoryKind::Buffer),
            category("Free space", 958_643, CategoryKind::Free),
        ],
        mcp_tools: Vec::new(),
        memory_files: Vec::new(),
        agents: Vec::new(),
        skills: vec![SkillUsage {
            name: "loop".into(),
            source: "Built-in".into(),
            plugin: None,
            tokens: 115,
        }],
    }
}

#[test]
fn the_markdown_table_is_written_word_for_word() {
    assert_eq!(
        markdown(&c1()),
        "## Context Usage\n\n**Model:** <model>  \n**Tokens:** 8.4k / 1m (1%)\n\n### Estimated usage by category\n\n| Category | Tokens | Percentage |\n|----------|--------|------------|\n| System prompt | 1.6k | 0.2% |\n| System tools | 4.7k | 0.5% |\n| System tools (deferred) | 2.1k | 0.2% |\n| Skills | 2k | 0.2% |\n| Messages | 21 | 0.0% |\n| Free space | 958.6k | 95.9% |\n| Autocompact buffer | 33k | 3.3% |\n\n### Skills\n\n| Skill | Source | Tokens |\n|-------|--------|--------|\n| loop | Built-in | ~120 |\n\n"
            .replace("<model>", GLM_5_3)
    );
}

#[test]
fn memory_files_and_the_over_limit_note_are_listed() {
    let mut usage = c1();
    usage.total_tokens = 1_002_000;
    usage.percentage = 100;
    usage.memory_files = vec![MemoryFileUsage {
        path: "/Users/dev/project/AGENTS.md".into(),
        kind: "Project".into(),
        tokens: 1234,
    }];
    usage.skills.clear();
    let text = markdown(&usage);
    assert!(text.contains("**Over limit:** Context is 2k tokens past the 1m-token compaction window \u{2014} run /compact to reduce usage.\n\n"), "{text}");
    assert!(text.ends_with("### Memory Files\n\n| Type | Path | Tokens |\n|------|------|--------|\n| Project | /Users/dev/project/AGENTS.md | 1.2k |\n\n"), "{text}");
    assert!(is_context("/context") && is_context(" /context all ") && !is_context("/contexts"));
}
