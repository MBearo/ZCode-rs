//! Agent 工具行与 Agent 的结果渲染（docs/specs/rust-cli/tui-display.md §2.1）。
//! 头是 `⏺ <名称>(<description>)`，运行中列出子代理最后几项活动，结束后是 `Done (…)` 或
//! `Backgrounded agent`。完成的统计从工具结果的 `<usage>` 尾注读（冷加载与实时同一个来源）。
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use super::{Ctx, DETAIL, INDENT, bold, detail_line, dim, dot, red, tools};
use crate::ui::i18n::Lang;
use crate::ui::retry_status;
use crate::ui::text::{truncate, truncate_line};
use crate::view::{Activity, AgentActivity, ToolRow, ToolStatus};

/// 紧凑模式下显示的活动项数。
const SHOWN: usize = 3;

pub(super) fn is_agent(tool: &ToolRow) -> bool {
    matches!(tool.name.as_str(), "Agent" | "Task")
}

/// 显示名：`subagent_type`，没有或是 general-purpose 时写 `Agent`。
fn display_name(input: &Value) -> String {
    match input["subagent_type"].as_str() {
        Some(kind) if !kind.is_empty() && kind != "general-purpose" && kind != "worker" => {
            kind.to_owned()
        }
        _ => "Agent".to_owned(),
    }
}

/// 数字与时长的写法：唯一定义在 kernel-api `metering`（F58：以前这里各写了
/// 一份；秒的取整原来是 f64 的 `round`，kernel-api 是整数的 `+ 500`，测试核对两者在 0–59999 上相同）。
pub(crate) use rust_cli_kernel_api::metering::{
    format_duration as duration, format_number as compact_number,
};

fn plural(count: usize, one: &str, many: &str) -> String {
    if count == 1 {
        format!("1 {one}")
    } else {
        format!("{count} {many}")
    }
}

/// 结果的种类：前台完成（带统计）或后台启动。
enum Ending {
    Done {
        tool_uses: u64,
        tokens: u64,
        duration_ms: u64,
    },
    Backgrounded,
}

/// 结局：先看工具给界面的 `display.agent`（实时）；冷加载时没有它，从结果文字读：后台启动的文字以
/// `Async agent launched` 开头，前台的 `<usage>` 尾注给出统计（agents 的 `handback_trailer.txt`；one-shot
/// agent 没有尾注，冷加载后按普通工具显示）。
fn ending(tool: &ToolRow) -> Option<Ending> {
    let display = &tool.display["agent"];
    match display["status"].as_str() {
        Some("async_launched") => return Some(Ending::Backgrounded),
        Some("completed") => {
            return Some(Ending::Done {
                tool_uses: display["toolUses"].as_u64().unwrap_or_default(),
                tokens: display["tokens"].as_u64().unwrap_or_default(),
                duration_ms: display["durationMs"].as_u64().unwrap_or_default(),
            });
        }
        _ => {}
    }
    let output = tool.output.as_str();
    if output.starts_with("Async agent launched") {
        return Some(Ending::Backgrounded);
    }
    let usage = output.rsplit_once("<usage>")?.1;
    let field = |name: &str| {
        usage.lines().find_map(|line| {
            let value = line.trim().strip_prefix(name)?.strip_prefix(':')?;
            value
                .trim()
                .trim_end_matches("</usage>")
                .parse::<u64>()
                .ok()
        })
    };
    Some(Ending::Done {
        tool_uses: field("tool_uses")?,
        tokens: field("subagent_tokens").or_else(|| field("total_tokens"))?,
        duration_ms: field("duration_ms")?,
    })
}

pub(super) fn expand_hint(lang: Lang) -> &'static str {
    match lang {
        Lang::En => "(ctrl+o to expand)",
        Lang::Zh => "（ctrl+o 展开）",
    }
}

/// 活动项的一行：工具 `名称(参数摘要)`，文字取第一行。
fn activity_line(item: &Activity, ctx: &Ctx<'_>) -> Line<'static> {
    let (text, style) = match item {
        Activity::Tool {
            name,
            input,
            status,
            id,
        } => {
            let row = ToolRow {
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                status: *status,
                output: String::new(),
                preview: String::new(),
                display: Value::Null,
                activity: None,
            };
            let (label, args, note) = tools::head_parts(&row, ctx);
            let text = match args {
                Some(args) => format!("{label}({args}){note}"),
                None => format!("{label}{note}"),
            };
            let style = if *status == ToolStatus::Error {
                red()
            } else {
                Style::default()
            };
            (text, style)
        }
        Activity::Text { text, .. } => (
            text.trim().lines().next().unwrap_or_default().to_owned(),
            Style::default(),
        ),
        // `∴ ` 加上推理文字，空白压成一个空格，dim 斜体。
        Activity::Thinking { text, .. } => (
            format!(
                "∴ {}",
                text.split_whitespace().collect::<Vec<_>>().join(" ")
            ),
            dim().add_modifier(Modifier::ITALIC),
        ),
    };
    let width = ctx.width.saturating_sub(DETAIL.chars().count());
    detail_line(truncate(&text, width), style)
}

/// 运行中的活动：没有时 `Initializing…`；紧凑模式只留最后 [`SHOWN`] 项，隐藏的工具调用数另起一行。
fn progress(activity: Option<&AgentActivity>, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let items: Vec<&Activity> = activity
        .map(|a| {
            a.items
                .iter()
                .filter(|i| {
                    !matches!(i, Activity::Text { text, .. } | Activity::Thinking { text, .. }
                        if text.trim().is_empty())
                })
                .collect()
        })
        .unwrap_or_default();
    if items.is_empty() {
        let text = match ctx.lang {
            Lang::En => "Initializing…",
            Lang::Zh => "初始化中…",
        };
        return vec![detail_line(text.to_owned(), dim())];
    }
    let hidden = if ctx.expanded {
        0
    } else {
        items.len().saturating_sub(SHOWN)
    };
    let mut lines: Vec<Line<'static>> = items[hidden..]
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut line = activity_line(item, ctx);
            // 第一项接在 `⎿` 后面，其余缩进对齐（同一块里的多行）。
            if index > 0 {
                line.spans[0] = Span::raw(INDENT);
            }
            line
        })
        .collect();
    let hidden_tools = items[..hidden]
        .iter()
        .filter(|item| matches!(item, Activity::Tool { .. }))
        .count();
    if hidden_tools > 0 {
        let more = match ctx.lang {
            Lang::En => format!(
                "… +{} {}",
                plural(hidden_tools, "tool use", "tool uses"),
                expand_hint(ctx.lang)
            ),
            Lang::Zh => format!("… +{hidden_tools} 次工具调用 {}", expand_hint(ctx.lang)),
        };
        lines.push(Line::from(Span::styled(format!("{INDENT}{more}"), dim())));
    }
    lines
}

/// 子代理的模型请求在等重试或停住时，活动之后加一行重试行（stall-visibility.md §3.2，与主线程的状态行同一套文字）。
fn retry_line(activity: Option<&AgentActivity>, ctx: &Ctx<'_>) -> Option<Line<'static>> {
    let notice = activity?.notice.as_ref()?;
    let width = ctx.width.saturating_sub(INDENT.chars().count());
    let mut spans = vec![Span::raw(INDENT)];
    spans.extend(retry_status::spans(notice, ctx.now_ms, ctx.lang, width));
    Some(truncate_line(Line::from(spans), ctx.width))
}

fn done_line(tool_uses: u64, tokens: u64, duration_ms: u64, lang: Lang) -> String {
    let uses = usize::try_from(tool_uses).unwrap_or(usize::MAX);
    match lang {
        Lang::En => format!(
            "Done ({} · {} tokens · {})",
            plural(uses, "tool use", "tool uses"),
            compact_number(tokens),
            duration(duration_ms)
        ),
        Lang::Zh => format!(
            "完成（{uses} 次工具调用 · {} tokens · {}）",
            compact_number(tokens),
            duration(duration_ms)
        ),
    }
}

pub(super) fn agent(tool: &ToolRow, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let name = display_name(&tool.input);
    let description = tool.input["description"].as_str().unwrap_or_default();
    let head = Line::from(vec![
        dot(tool.status.into(), ctx),
        Span::raw(" "),
        Span::styled(name, bold()),
        Span::raw(format!("({description})")),
    ]);
    let mut lines = vec![truncate_line(head, ctx.width)];
    match tool.status {
        ToolStatus::Running | ToolStatus::Pending | ToolStatus::Approval => {
            lines.extend(progress(tool.activity.as_ref(), ctx));
            lines.extend(retry_line(tool.activity.as_ref(), ctx));
        }
        ToolStatus::Success => match ending(tool) {
            Some(Ending::Backgrounded) => {
                let text = match ctx.lang {
                    Lang::En => "Backgrounded agent (↓ to manage · ctrl+o to expand)",
                    Lang::Zh => "已转到后台（↓ 管理 · ctrl+o 展开）",
                };
                lines.push(detail_line(text.to_owned(), Style::default()));
            }
            Some(Ending::Done {
                tool_uses,
                tokens,
                duration_ms,
            }) => {
                if ctx.expanded {
                    lines.extend(progress(tool.activity.as_ref(), ctx));
                }
                let done = done_line(tool_uses, tokens, duration_ms, ctx.lang);
                lines.push(detail_line(done, Style::default()));
                if !ctx.expanded {
                    lines.push(Line::from(Span::styled(
                        format!("  {}", expand_hint(ctx.lang)),
                        dim(),
                    )));
                }
            }
            // 认不出的结果（one-shot agent 没有尾注）：按普通工具显示输出。
            None => return tools::plain(tool, ctx),
        },
        ToolStatus::Error | ToolStatus::Cancelled => return tools::plain(tool, ctx),
    }
    lines
}

#[cfg(test)]
#[path = "agent_tests.rs"]
mod tests;
