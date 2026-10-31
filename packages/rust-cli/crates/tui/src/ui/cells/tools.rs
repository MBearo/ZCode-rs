// 改写自 apps/zcode-host/crates/tui/src/ui/cells/tools.rs（feat/rs 79b6691c55）：输入换成 `view::ToolRow`。
//! 工具类的块：`⏺ 名称(参数)`，细节挂在 `  ⎿  ` 下面。文件改动的细节见 `patch`。
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use serde_json::Value;

use super::words::{self, Counted};
use super::{
    Ctx, DETAIL, Dot, INDENT, bold, detail_line, detail_text, dim, dot, more_lines, output_lines,
    patch, red, shown_path,
};
use crate::ui::markdown;
use crate::ui::text::{truncate, truncate_line, wrap};
use crate::view::{ToolRow, ToolStatus};
use rust_cli_kernel_api::mcp_state::WAIT_FOR_MCP_SERVERS;

/// 紧凑模式下输出保留开头的行数。
const HEAD: usize = 3;
/// 详细模式下最多显示的输出行数。
const EXPANDED_LIMIT: usize = 400;
/// 紧凑模式下命令参数最多的字符数。
const COMMAND_LIMIT: usize = 160;

pub(super) const PATCH_TOOLS: [&str; 4] = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// 参数摘要：`键: 值, 键: 值`（按 JSON 写值，字符串带引号）。远端的 `machine` 不写在这里，接在行尾（`@机器`，
/// remote-exec.md §21.6）。
fn args_summary(args: &Value) -> String {
    match args {
        Value::Object(map) => map
            .iter()
            .filter(|(key, _)| {
                !(key.as_str() == rust_cli_kernel_api::machine_param::MACHINE
                    && crate::ui::machines::machine_of(args).is_some())
            })
            .map(|(key, value)| format!("{key}: {value}"))
            .collect::<Vec<_>>()
            .join(", "),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// WaitForMcpServers 的显示名。
const WAIT_LABEL: &str = "MCP Wait For Servers";

/// WaitForMcpServers 的参数位置。
fn wait_for_servers(args: &Value) -> String {
    let servers: Vec<&str> = args["servers"]
        .as_array()
        .map(|servers| servers.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    if servers.is_empty() {
        "Wait for pending MCP servers to connect".to_owned()
    } else {
        format!("Wait for MCP servers to connect: {}", servers.join(", "))
    }
}

/// 名称、参数与名称后的附注（MCP 的 ` (MCP)`）。参数为 None 时不写括号。
pub(super) fn head_parts(tool: &ToolRow, ctx: &Ctx<'_>) -> (String, Option<String>, &'static str) {
    let name = tool.name.as_str();
    let args = &tool.input;
    let path = |keys: &[&str]| {
        keys.iter()
            .find_map(|key| args[*key].as_str())
            .map(|path| shown_path(path, tool, ctx))
    };
    // R5-1：输入不是合法 JSON（标记对象）时参数位置写固定的说明，不按工具解析参数。
    if let Some(unparsed) = rust_cli_kernel_api::unparsed_input::parse(args) {
        let text = format!("input JSON failed to parse \u{2014} {} bytes", unparsed.len);
        return (name.to_owned(), Some(text), "");
    }
    if let Some(rest) = name.strip_prefix("mcp__") {
        let label = rest.replacen("__", ".", 1);
        return (label, Some(args_summary(args)), " (MCP)");
    }
    // WaitForMcpServers（ext.md §3.8，修复线 F36）：显示名与参数位置。
    if name == WAIT_FOR_MCP_SERVERS {
        return (WAIT_LABEL.to_owned(), Some(wait_for_servers(args)), "");
    }
    let args_text = match name {
        "Read" => path(&["file_path", "path"]).map(|mut path| {
            if ctx.expanded
                && let Some(offset) = args["offset"].as_u64()
            {
                let end = args["limit"].as_u64().map_or(String::new(), |limit| {
                    (offset + limit).saturating_sub(1).to_string()
                });
                path.push_str(&format!(" · lines {offset}-{end}"));
            }
            path
        }),
        "Bash" => {
            let command = args["command"].as_str().unwrap_or_default();
            Some(if ctx.expanded {
                command.to_owned()
            } else {
                let first = command.lines().next().unwrap_or_default();
                let mut text: String = first.chars().take(COMMAND_LIMIT).collect();
                if command.lines().nth(1).is_some() || first.chars().count() > COMMAND_LIMIT {
                    text.push_str(" …");
                }
                text
            })
        }
        "Grep" | "Glob" => {
            let mut text = format!("pattern: {}", args["pattern"]);
            if let Some(path) = path(&["path"]) {
                text.push_str(&format!(", path: \"{path}\""));
            }
            Some(text)
        }
        "List" | "LS" => Some(path(&["path"]).unwrap_or_else(|| ".".into())),
        "WebSearch" => args["query"].as_str().map(str::to_owned),
        "WebFetch" => args["url"].as_str().map(str::to_owned),
        "AskUserQuestion" => None,
        _ if PATCH_TOOLS.contains(&name) => path(&["file_path", "notebook_path", "path"]),
        _ => Some(args_summary(args)),
    };
    (name.to_owned(), args_text, "")
}

/// 这次调用在哪台远端机器上：输入的 `machine`、MCP 服务器的绑定，或结果的 `display.machine`（TaskStop 停的远端命令，
/// F98，remote-exec.md §25.4.5）。
fn remote_machine<'a>(tool: &'a ToolRow, ctx: &'a Ctx<'_>) -> Option<&'a str> {
    crate::ui::machines::remote_of(&tool.name, &tool.input, ctx.mcp_machines)
        .or_else(|| tool.display["machine"].as_str())
}

/// 第一行：圆点、粗体的名称、`(参数)`。紧凑模式截断参数；详细模式完整显示，续行缩进 2 列。远端的调用在行尾接
/// `  @机器`（remote-exec.md §21.6；绑定到远端机器的 MCP 服务器的工具也是，§22.8）。
fn head(tool: &ToolRow, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    /// 机器标记前面空两列。
    const TAG_GAP: &str = "  ";
    let mut lines = head_without_machine(tool, ctx);
    if let (Some(machine), Some(last)) = (remote_machine(tool, ctx), lines.last_mut()) {
        let tag = crate::ui::machines::tag(ctx.palette, machine);
        let tag_width = unicode_width::UnicodeWidthStr::width(tag.content.as_ref());
        let room = ctx.width.saturating_sub(last.width());
        if room >= TAG_GAP.len() + tag_width {
            last.spans.push(Span::raw(TAG_GAP));
            last.spans.push(tag);
        } else if !ctx.expanded {
            // 紧凑模式放不下：截掉参数的末尾给标记让位。
            let keep = ctx.width.saturating_sub(TAG_GAP.len() + tag_width);
            *last = truncate_line(std::mem::take(last), keep);
            last.spans.push(Span::raw(TAG_GAP));
            last.spans.push(tag);
        } else {
            lines.push(Line::from(vec![Span::raw(TAG_GAP), tag]));
        }
    }
    lines
}

fn head_without_machine(tool: &ToolRow, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let (name, args, note) = head_parts(tool, ctx);
    let lead = vec![
        dot(Dot::from(tool.status), ctx),
        Span::raw(" "),
        Span::styled(name.clone(), bold()),
    ];
    let Some(args) = args else {
        let mut spans = lead;
        spans.push(Span::styled(note, dim()));
        return vec![truncate_line(Line::from(spans), ctx.width)];
    };
    if !ctx.expanded {
        // 圆点与空格 2 列、名称、括号 2 列、说明。
        const LEAD: usize = 2;
        let used =
            LEAD + unicode_width::UnicodeWidthStr::width(name.as_str()) + "()".len() + note.len();
        let mut spans = lead;
        spans.push(Span::raw(format!(
            "({})",
            truncate(&args, ctx.width.saturating_sub(used))
        )));
        spans.push(Span::styled(note, dim()));
        return vec![truncate_line(Line::from(spans), ctx.width)];
    }
    let parts: Vec<&str> = args.split('\n').collect();
    let last = parts.len() - 1;
    let mut lines = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let mut spans = if index == 0 {
            let mut spans = lead.clone();
            spans.push(Span::raw("("));
            spans
        } else {
            Vec::new()
        };
        spans.push(Span::raw((*part).to_owned()));
        if index == last {
            spans.push(Span::raw(")"));
            spans.push(Span::styled(note, dim()));
        }
        let first: &[Span<'static>] = if index == 0 { &[] } else { &[Span::raw("  ")] };
        lines.extend(wrap(&spans, ctx.width, first, &[Span::raw("  ")]));
    }
    lines
}

/// 输出块：第一行 `  ⎿  `，其余缩进 5 列。紧凑模式只留开头 3 行（只多 1 行时全部显示），其余写展开行；
/// 详细模式全部显示（最多 [`EXPANDED_LIMIT`] 行）并换行。
fn output_block(lines: &[String], style: Style, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let (shown, hidden) = if ctx.expanded {
        let shown = lines.len().min(EXPANDED_LIMIT);
        (shown, lines.len() - shown)
    } else if lines.len() > HEAD + 1 {
        (HEAD, lines.len() - HEAD)
    } else {
        (lines.len(), 0)
    };
    let mut out = Vec::new();
    for (index, text) in lines[..shown].iter().enumerate() {
        let prefix = Span::styled(if index == 0 { DETAIL } else { INDENT }, dim());
        if ctx.expanded {
            out.extend(wrap(
                &[Span::styled(text.clone(), style)],
                ctx.width,
                &[prefix],
                &[Span::raw(INDENT)],
            ));
        } else {
            // 紧凑模式不换行：一行只占一行，超出截断（终端输出常有很长的行）。
            let line = Line::from(vec![prefix, Span::styled(text.clone(), style)]);
            out.push(truncate_line(line, ctx.width));
        }
    }
    if hidden > 0 {
        out.push(more_lines(hidden, ctx));
    }
    out
}

/// `前文 **数字** 后文` 的细节行。
fn counted_line((before, count, after): Counted) -> Line<'static> {
    Line::from(vec![
        Span::styled(DETAIL, dim()),
        Span::raw(before),
        Span::styled(count, bold()),
        Span::raw(after),
    ])
}

/// 搜索类工具的计数：（数，单位）。数从输出取。
fn search_count(tool: &ToolRow, lines: &[String]) -> (u64, &'static str) {
    let name = tool.name.as_str();
    let first = lines.first().map(String::as_str).unwrap_or_default();
    let leading = |line: &str, word: &str| {
        line.strip_prefix(word)
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|n| n.parse::<u64>().ok())
    };
    let non_empty = |skip: &dyn Fn(&str) -> bool| {
        lines
            .iter()
            .filter(|line| !line.trim().is_empty() && !skip(line))
            .count() as u64
    };
    if matches!(name, "List" | "LS") {
        return (non_empty(&|_| false), "paths");
    }
    match tool.input["output_mode"].as_str() {
        Some("content") if name == "Grep" => {
            if first == "No matches found" {
                return (0, "lines");
            }
            (
                non_empty(&|line| line.starts_with("[Showing results")),
                "lines",
            )
        }
        Some("count") if name == "Grep" => {
            let total = lines.iter().find_map(|line| leading(line, "Found "));
            (total.unwrap_or_else(|| non_empty(&|_| false)), "matches")
        }
        _ => {
            if first.starts_with("No files found") || first.starts_with("No matches") {
                return (0, "files");
            }
            let count = leading(first, "Found ")
                .unwrap_or_else(|| non_empty(&|line| line.starts_with("Found ")));
            (count, "files")
        }
    }
}

/// AskUserQuestion 的答案：从输出里的 `"问题"="答案"` 取。
fn answers(tool: &ToolRow) -> Vec<(String, String)> {
    let output = tool.output.as_str();
    tool.input["questions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|q| {
            let question = q["question"].as_str()?;
            let pattern = format!("\"{question}\"=\"");
            let start = output.find(&pattern)? + pattern.len();
            let answer = &output[start..];
            let answer = &answer[..answer.find('"').unwrap_or(answer.len())];
            Some((question.to_owned(), answer.to_owned()))
        })
        .collect()
}

/// ExitPlanMode 结束后：`⏺ User approved ZCode's plan` / `rejected`；详细模式下面是计划全文。
fn plan(tool: &ToolRow, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let approved = tool.status == ToolStatus::Success;
    let mut lines = vec![Line::from(vec![
        dot(Dot::from(tool.status), ctx),
        Span::raw(" "),
        Span::raw(words::plan(ctx.lang, approved)),
    ])];
    if ctx.expanded
        && let Some(text) = tool.input["plan"].as_str()
    {
        lines.extend(markdown::render(
            text,
            ctx.width,
            vec![Span::raw(INDENT)],
            vec![Span::raw(INDENT)],
            Style::default(),
            ctx.palette,
        ));
    }
    lines
}

pub fn tool(tool: &ToolRow, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    // Agent 工具另有排版：子代理进度、Done 与 Backgrounded（tui-display.md §2.1）。
    if super::agent::is_agent(tool) {
        return super::agent::agent(tool, ctx);
    }
    plain(tool, ctx)
}

/// 普通工具行（Agent 认不出结果、失败或中断时也用它）。
pub(super) fn plain(tool: &ToolRow, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    if tool.name == "ExitPlanMode" && !tool.status.is_live() {
        return plan(tool, ctx);
    }
    let mut lines = head(tool, ctx);
    let output = output_lines(tool);
    // 远端的调用在等重连中的机器（F98，remote-exec.md §25.4.1，Docker 试用 L3）：写明在等什么，不只写“运行中…”。
    let waiting = remote_machine(tool, ctx)
        .filter(|machine| {
            tool.status.is_live()
                && tool.status != ToolStatus::Approval
                && crate::ui::machines::reconnecting(ctx.machines, machine)
        })
        .map(|machine| crate::ui::i18n_machines::waiting_reconnect(ctx.lang, machine));
    if let Some(waiting) = waiting {
        if !output.is_empty() {
            let tail = &output[output.len().saturating_sub(HEAD)..];
            let compact = Ctx {
                expanded: false,
                ..*ctx
            };
            lines.extend(output_block(tail, dim(), &compact));
        }
        lines.push(detail_line(waiting, dim()));
        return lines;
    }
    match tool.status {
        ToolStatus::Approval => lines.push(detail_line(words::waiting(ctx.lang).into(), dim())),
        ToolStatus::Cancelled => lines.push(detail_line(words::interrupted(ctx.lang), dim())),
        ToolStatus::Error => lines.extend(output_block(&output, red(), ctx)),
        ToolStatus::Running if output.is_empty() && tool.name == "Bash" => {
            lines.push(detail_line(words::running(ctx.lang).into(), dim()));
        }
        ToolStatus::Running | ToolStatus::Pending if output.is_empty() => {}
        ToolStatus::Running | ToolStatus::Pending => {
            let tail = &output[output.len().saturating_sub(HEAD)..];
            let ctx = Ctx {
                expanded: false,
                ..*ctx
            };
            lines.extend(output_block(tail, dim(), &ctx));
        }
        ToolStatus::Success => lines.extend(details(tool, &output, ctx)),
    }
    lines
}

/// 成功后的细节行。
fn details(tool: &ToolRow, output: &[String], ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let name = tool.name.as_str();
    if PATCH_TOOLS.contains(&name) {
        return patch::details(tool, ctx);
    }
    match name {
        "Read" => {
            let numbered = output
                .iter()
                .filter(|line| {
                    let digits = line.trim_start().split('\t').next().unwrap_or_default();
                    line.contains('\t')
                        && !digits.is_empty()
                        && digits.bytes().all(|b| b.is_ascii_digit())
                })
                .count() as u64;
            match output.first() {
                Some(first) if numbered == 0 && !first.starts_with('<') => {
                    vec![detail_line(first.clone(), Style::default())]
                }
                _ => vec![counted_line(words::read_lines(ctx.lang, numbered))],
            }
        }
        "Grep" | "Glob" | "List" | "LS" => {
            let (count, unit) = search_count(tool, output);
            vec![counted_line(words::found(ctx.lang, count, unit))]
        }
        "AskUserQuestion" => answers(tool)
            .into_iter()
            .flat_map(|(question, answer)| {
                detail_text(&format!("· {question} → {answer}"), Style::default(), ctx)
            })
            .collect(),
        "Bash" if output.is_empty() => {
            vec![detail_line(words::no_output(ctx.lang).into(), dim())]
        }
        // 没有结果的渲染：成功时不画结果（失败照常画错误文字）。
        WAIT_FOR_MCP_SERVERS => Vec::new(),
        _ => output_block(output, dim(), ctx),
    }
}

#[cfg(test)]
#[path = "tools_tests.rs"]
mod tests;
