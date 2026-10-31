// 改写自 apps/zcode-host/crates/tui/src/ui/cells/group.rs（feat/rs 79b6691c55）：输入换成视图行。
//! 工作段（rust-tui.md §8.2.1）：连续的推理与读取类工具并成一个块。紧凑形式是一行摘要（进行中再加
//! 最近一项），详细形式是每个成员的详细形式。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use rust_cli_base::units;

use super::words::{self, Segment};
use super::{Cell, Ctx, DETAIL, Dot, INDENT, dim, dot, messages, relative, tools};
use crate::ui::i18n::Lang;
use crate::ui::text::{truncate_line, wrap, wrap_text};
use crate::view::{Body, Row};

/// 单段推理计入的时长上限（10 分钟）。
const THOUGHT_CAP_MS: u64 = 600_000;
/// 进行中的推理在摘要下面显示的行数。
const THOUGHT_TAIL: usize = 10;
/// 并进工作段、但不计数的工具。
const SILENT: [&str; 2] = ["TodoRead", "ToolSearch"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Member {
    Thought,
    Search,
    Read,
    List,
    Mcp,
    Command,
    Silent,
}

fn member(row: &Row) -> Option<Member> {
    let tool = match &row.body {
        Body::Reasoning { .. } => return Some(Member::Thought),
        Body::Tool(tool) => tool,
        _ => return None,
    };
    // 远端的调用不并进工作段（remote-exec.md §21.6，暂定）：单独一行，行尾标出机器。
    if crate::ui::machines::machine_of(&tool.input).is_some() {
        return None;
    }
    if tool.name.starts_with("mcp__") {
        return Some(Member::Mcp);
    }
    match tool.name.as_str() {
        "Grep" | "Glob" => Some(Member::Search),
        "Read" => Some(Member::Read),
        "List" | "LS" => Some(Member::List),
        "Bash" => Some(Member::Command),
        name if SILENT.contains(&name) => Some(Member::Silent),
        _ => None,
    }
}

/// 行能不能并进工作段（文字为空的推理由调用方跳过）。`mcp` 是绑定到远端机器的 MCP 服务器：它们的调用同远端的调用，
/// 不并进工作段（R5，remote-exec.md §22.8）。
pub(super) fn joins(row: &Row, mcp: &[(String, String)]) -> bool {
    if let Body::Tool(tool) = &row.body
        && crate::ui::machines::remote_of(&tool.name, &tool.input, mcp).is_some()
    {
        return false;
    }
    member(row).is_some()
}

/// 已结束的推理用了多久：记下的用时；没有时到下一行出现的时间。流式中的推理在排版时按当前时间算，
/// 这里不计。
pub(super) fn thought_ms(row: &Row, next_created: Option<u64>) -> u64 {
    let ms = match row.body {
        Body::Reasoning {
            streaming: true, ..
        } => return 0,
        Body::Reasoning {
            duration_ms: Some(ms),
            ..
        } => ms,
        _ => next_created.map_or(0, |next| next.saturating_sub(row.created_ms)),
    };
    ms.min(THOUGHT_CAP_MS)
}

/// MCP 工具的服务名（`mcp__服务__工具`）。
fn server(name: &str) -> String {
    let rest = name.trim_start_matches("mcp__");
    rest.split("__").next().unwrap_or(rest).to_owned()
}

/// 摘要的各段，按 rust-tui.md §8.2.1 的顺序。
fn segments(cell: &Cell<'_>, now_ms: u64) -> Vec<Segment> {
    let mut thought = None::<u64>;
    let (mut searches, mut lists, mut commands) = (0u64, 0u64, 0u64);
    let mut files: Vec<String> = Vec::new();
    let mut servers: Vec<(String, u64)> = Vec::new();
    for row in &cell.rows {
        match member(row) {
            Some(Member::Thought) => {
                let live = if matches!(
                    row.body,
                    Body::Reasoning {
                        streaming: true,
                        ..
                    }
                ) {
                    now_ms.saturating_sub(row.created_ms).min(THOUGHT_CAP_MS)
                } else {
                    0
                };
                *thought.get_or_insert(cell.thought_ms) += live;
            }
            Some(Member::Search) => searches += 1,
            Some(Member::Read) => {
                let args = row.tool().map(|tool| &tool.input);
                let path = args
                    .and_then(|args| args["file_path"].as_str().or_else(|| args["path"].as_str()))
                    .unwrap_or_default()
                    .to_owned();
                if !files.contains(&path) {
                    files.push(path);
                }
            }
            Some(Member::List) => lists += 1,
            Some(Member::Mcp) => {
                let name = server(row.tool().map_or("", |tool| tool.name.as_str()));
                match servers.iter_mut().find(|(s, _)| *s == name) {
                    Some((_, count)) => *count += 1,
                    None => servers.push((name, 1)),
                }
            }
            Some(Member::Command) => commands += 1,
            Some(Member::Silent) | None => {}
        }
    }
    let mut out = Vec::new();
    if let Some(ms) = thought {
        out.push(Segment::Thought(crate::ui::i18n::elapsed_plain(
            ms.max(units::MS_PER_SECOND).div_ceil(units::MS_PER_SECOND),
        )));
    }
    if searches > 0 {
        out.push(Segment::Search(searches));
    }
    if !files.is_empty() {
        out.push(Segment::Read(files.len() as u64));
    }
    if lists > 0 {
        out.push(Segment::List(lists));
    }
    out.extend(
        servers
            .into_iter()
            .map(|(name, count)| Segment::Mcp(name, count)),
    );
    if commands > 0 {
        out.push(Segment::Command(commands));
    }
    out
}

fn capitalize(text: &str) -> String {
    let mut chars = text.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_uppercase().chain(chars).collect()
    })
}

/// 紧凑形式：一行摘要（按宽度换行，续行缩进 2 列）；进行中再加最近一项。没有可写的段时为空。
pub(super) fn summary(cell: &Cell<'_>, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let parts = segments(cell, ctx.now_ms);
    if parts.is_empty() {
        return Vec::new();
    }
    let active = cell.live();
    let base = if active { Style::default() } else { dim() };
    let mut spans = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let (before, number, after) = words::segment(ctx.lang, active, part);
        if index > 0 {
            spans.push(Span::styled(words::joiner(ctx.lang), base));
        }
        let before = if index == 0 && ctx.lang == Lang::En {
            capitalize(&before)
        } else {
            before
        };
        spans.push(Span::styled(before, base));
        if !number.is_empty() {
            spans.push(Span::styled(number, base.add_modifier(Modifier::BOLD)));
        }
        if !after.is_empty() {
            spans.push(Span::styled(after, base));
        }
    }
    if active {
        spans.push(Span::styled("…", base));
    }
    let lead = if active {
        vec![dot(Dot::Live, ctx), Span::raw(" ")]
    } else {
        vec![Span::raw("  ")]
    };
    let mut lines = wrap(&spans, ctx.width, &lead, &[Span::raw("  ")]);
    if active {
        lines.extend(latest(cell, ctx));
    }
    lines
}

/// 进行中的工作段的最近一项（dim）：推理是正文的最后几行（斜体），工具是命令、路径或模式。
fn latest(cell: &Cell<'_>, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let Some(row) = cell
        .rows
        .iter()
        .rev()
        .find(|row| member(row) != Some(Member::Silent))
    else {
        return Vec::new();
    };
    let tool = match &row.body {
        Body::Reasoning { text, .. } => {
            let style = dim().add_modifier(Modifier::ITALIC);
            let mut lines = wrap_text(
                text.trim(),
                style,
                ctx.width,
                &[Span::raw(INDENT)],
                &[Span::raw(INDENT)],
            );
            let mut tail = lines.split_off(lines.len().saturating_sub(THOUGHT_TAIL));
            if let Some(first) = tail.first_mut().and_then(|line| line.spans.first_mut()) {
                *first = Span::styled(DETAIL, dim());
            }
            return tail;
        }
        Body::Tool(tool) => tool,
        _ => return Vec::new(),
    };
    let args = &tool.input;
    let path = |key: &str| args[key].as_str().map(|path| relative(path, ctx.workspace));
    let text = match member(row) {
        Some(Member::Command) => format!(
            "$ {}",
            args["command"]
                .as_str()
                .unwrap_or_default()
                .lines()
                .next()
                .unwrap_or_default()
        ),
        Some(Member::Read) => path("file_path")
            .or_else(|| path("path"))
            .unwrap_or_default(),
        Some(Member::List) => path("path").unwrap_or_else(|| ".".into()),
        Some(Member::Search) => format!("\"{}\"", args["pattern"].as_str().unwrap_or_default()),
        Some(Member::Mcp) => tool.name.trim_start_matches("mcp__").replacen("__", ".", 1),
        _ => return Vec::new(),
    };
    vec![truncate_line(
        Line::from(vec![Span::styled(DETAIL, dim()), Span::styled(text, dim())]),
        ctx.width,
    )]
}

/// 详细形式：每个成员的详细形式，成员之间空 1 行。
pub(super) fn expanded(cell: &Cell<'_>, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let ctx = Ctx {
        expanded: true,
        ..*ctx
    };
    let mut out: Vec<Line<'static>> = Vec::new();
    for row in &cell.rows {
        let lines = match &row.body {
            Body::Reasoning { text, .. } => messages::reasoning(text, &ctx),
            Body::Tool(tool) => tools::tool(tool, &ctx),
            _ => Vec::new(),
        };
        if lines.is_empty() {
            continue;
        }
        if !out.is_empty() {
            out.push(Line::default());
        }
        out.extend(lines);
    }
    out
}

#[cfg(test)]
#[path = "group_tests.rs"]
mod tests;
