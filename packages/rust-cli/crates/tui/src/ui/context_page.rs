//! `/context` 独立页面的正文（docs/specs/rust-cli/tui-panels.md §4；Host `rust-tui-context-usage.md` §4.3–§4.4）：
//! 方格图之后是明细（默认折叠，`/context all` 展开）与建议。数据都来自内核的 [`ContextUsage`]，不另算。
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};
use rust_cli_kernel_api::context_usage::{ContextUsage, format_estimate, format_tokens};

use super::cells::{self, Ctx};
use super::i18n::Lang;

/// 明细的一段：标题、条目与合计。
struct Section {
    title: &'static str,
    /// 折叠时的单位（单数、复数、中文量词）。
    unit: (&'static str, &'static str, &'static str),
    rows: Vec<String>,
    tokens: u64,
}

fn sections(usage: &ContextUsage) -> Vec<Section> {
    // 顺序：MCP 工具、自定义 agent、规则文件、skill。
    vec![
        Section {
            title: "MCP tools \u{b7} /mcp",
            unit: ("tool", "tools", "个工具"),
            rows: usage
                .mcp_tools
                .iter()
                .map(|tool| format!("{}: {} tokens", tool.name, format_tokens(tool.tokens)))
                .collect(),
            tokens: usage.mcp_tools.iter().map(|tool| tool.tokens).sum(),
        },
        Section {
            // 原文是 `Custom agents · /agents`；`/agents` 已删（L33），只留类别名，不指向不存在的命令。
            title: "Custom agents",
            unit: ("agent", "agents", "个 agent"),
            rows: usage
                .agents
                .iter()
                .map(|agent| {
                    format!(
                        "{} ({}): {} tokens",
                        agent.agent_type,
                        agent.source,
                        format_tokens(agent.tokens)
                    )
                })
                .collect(),
            tokens: usage.agents.iter().map(|agent| agent.tokens).sum(),
        },
        Section {
            title: "Memory files \u{b7} /memory",
            unit: ("file", "files", "个文件"),
            rows: usage
                .memory_files
                .iter()
                .map(|file| {
                    format!(
                        "{} ({}): {} tokens",
                        file.kind,
                        file.path,
                        format_tokens(file.tokens)
                    )
                })
                .collect(),
            tokens: usage.memory_files.iter().map(|file| file.tokens).sum(),
        },
        Section {
            title: "Skills \u{b7} /skills",
            unit: ("skill", "skills", "个 skill"),
            rows: usage
                .skills
                .iter()
                .map(|skill| {
                    let source = match &skill.plugin {
                        Some(plugin) => format!("{} ({plugin})", skill.source),
                        None => skill.source.clone(),
                    };
                    format!(
                        "{} ({source}): {} tokens",
                        skill.name,
                        format_estimate(skill.tokens)
                    )
                })
                .collect(),
            tokens: usage.skills.iter().map(|skill| skill.tokens).sum(),
        },
    ]
}

/// 明细：每段一个粗体标题；折叠时一行 `└ N 个 · X tokens`，展开时每项一行（`├`，最后一项 `└`）。
fn details(usage: &ContextUsage, all: bool, lang: Lang) -> Vec<Line<'static>> {
    let dim = cells::dim();
    let mut lines = Vec::new();
    for section in sections(usage).into_iter().filter(|s| !s.rows.is_empty()) {
        lines.push(Line::default());
        lines.push(Line::from(Span::styled(section.title, cells::bold())));
        if all {
            let last = section.rows.len() - 1;
            for (index, row) in section.rows.into_iter().enumerate() {
                let branch = if index == last {
                    "\u{2514} "
                } else {
                    "\u{251c} "
                };
                lines.push(Line::from(vec![Span::styled(branch, dim), Span::raw(row)]));
            }
            continue;
        }
        let n = section.rows.len();
        let (one, many, zh) = section.unit;
        let count = match lang {
            Lang::En if n == 1 => format!("1 {one}"),
            Lang::En => format!("{n} {many}"),
            Lang::Zh => format!("{n} {zh}"),
        };
        lines.push(Line::from(vec![
            Span::styled("\u{2514} ", dim),
            Span::raw(format!(
                "{count} \u{b7} {} tokens",
                format_tokens(section.tokens)
            )),
        ]));
    }
    if !all && !lines.is_empty() {
        lines.push(Line::from(Span::styled(
            match lang {
                Lang::En => "/context all to expand",
                Lang::Zh => "/context all 展开明细",
            },
            dim,
        )));
    }
    lines
}

/// 一条建议：是不是警告、标题、能省多少、说明。
struct Suggestion {
    warning: bool,
    title: String,
    save: Option<u64>,
    detail: String,
}

/// 占窗口的百分比（取整）。
fn share(tokens: u64, window: u64) -> u64 {
    if window == 0 {
        return 0;
    }
    (tokens * 100 + window / 2) / window
}

/// 能从 `ContextUsage` 算出的建议（Host §4.4 的“接近满”与“规则文件”；按工具的结果用量内核没有，D6）。
fn suggestions(usage: &ContextUsage, lang: Lang) -> Vec<Suggestion> {
    let window = usage.raw_max_tokens;
    let mut out = Vec::new();
    if usage.total_tokens <= window && usage.percentage >= 80 {
        let detail = match (lang, usage.auto_compact) {
            (Lang::En, true) => {
                "Autocompact will trigger soon, which discards older messages. Use /compact now to control what gets kept."
            }
            (Lang::En, false) => "Autocompact is disabled. Use /compact to free space.",
            (Lang::Zh, true) => {
                "即将自动压缩，较早的消息会被丢弃。现在运行 /compact 可以控制保留哪些内容。"
            }
            (Lang::Zh, false) => "自动压缩已关闭。运行 /compact 释放空间。",
        };
        out.push(Suggestion {
            warning: true,
            title: match lang {
                Lang::En => format!("Context is {}% full", usage.percentage),
                Lang::Zh => format!("上下文已用 {}%", usage.percentage),
            },
            save: None,
            detail: detail.to_owned(),
        });
    }
    let memory: u64 = usage.memory_files.iter().map(|file| file.tokens).sum();
    if memory >= 5_000 && memory * 100 >= window * 5 {
        let mut largest: Vec<_> = usage.memory_files.iter().collect();
        largest.sort_by_key(|file| std::cmp::Reverse(file.tokens));
        let names: Vec<String> = largest
            .iter()
            .take(3)
            .map(|file| format!("{} ({})", file.path, format_tokens(file.tokens)))
            .collect();
        let (tokens, percent) = (format_tokens(memory), share(memory, window));
        out.push(Suggestion {
            warning: false,
            title: match lang {
                Lang::En => format!("Memory files using {tokens} tokens ({percent}%)"),
                Lang::Zh => format!("规则文件占用 {tokens} tokens（{percent}%）"),
            },
            save: Some(memory * 3 / 10),
            detail: match lang {
                Lang::En => format!(
                    "Largest: {}. Review and prune stale entries.",
                    names.join(", ")
                ),
                Lang::Zh => format!("最大的：{}。检查并删掉过时的内容。", names.join("、")),
            },
        });
    }
    // 先列警告，再按节省量从大到小。
    out.sort_by_key(|s| (!s.warning, std::cmp::Reverse(s.save.unwrap_or(0))));
    out
}

fn suggestion_lines(usage: &ContextUsage, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let list = suggestions(usage, ctx.lang);
    if list.is_empty() {
        return Vec::new();
    }
    let dim = cells::dim();
    let title = match ctx.lang {
        Lang::En => "Suggestions",
        Lang::Zh => "建议",
    };
    let mut lines = vec![
        Line::default(),
        Line::from(Span::styled(title, cells::bold())),
    ];
    for (index, item) in list.into_iter().enumerate() {
        if index > 0 {
            lines.push(Line::default());
        }
        let (icon, color) = if item.warning {
            ("\u{26a0} ", Color::Yellow)
        } else {
            ("\u{2139} ", ctx.palette.accent)
        };
        let mut spans = vec![
            Span::styled(icon, Style::default().fg(color)),
            Span::raw(item.title),
        ];
        if let Some(save) = item.save {
            let text = match ctx.lang {
                Lang::En => format!(" \u{2192} save ~{}", format_tokens(save)),
                Lang::Zh => format!(" \u{2192} 可省约 {}", format_tokens(save)),
            };
            spans.push(Span::styled(text, dim));
        }
        lines.push(Line::from(spans));
        // 说明缩进 2 列（dim）。
        lines.extend(super::text::wrap_text(
            &item.detail,
            dim,
            ctx.width,
            &[Span::raw("  ")],
            &[Span::raw("  ")],
        ));
    }
    lines
}

/// 页面的正文：方格图与说明、明细（`all` 时展开）、建议；与页面标题对齐，左边空 2 列。
pub fn page_lines(usage: &ContextUsage, all: bool, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let narrower = Ctx {
        width: ctx.width.saturating_sub(2),
        ..*ctx
    };
    let mut lines = super::context::chart(usage, &narrower);
    lines.extend(details(usage, all, ctx.lang));
    lines.extend(suggestion_lines(usage, &narrower));
    lines
        .into_iter()
        .map(|mut line| {
            line.spans.insert(0, Span::raw("  "));
            line
        })
        .collect()
}

#[cfg(test)]
#[path = "context_page_tests.rs"]
mod tests;
