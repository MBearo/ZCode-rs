// 改写自 apps/zcode-host/crates/tui/src/ui/cells/patch.rs（feat/rs 79b6691c55）：本工程的工具结果没有
// `structuredPatch`，diff 从工具输入算（Edit 的 old_string / new_string，Write 的 content），不带行号。
//! `/diff` 页的 diff 行（transcript 里改动的行由 kit 的 `tool_row::diff_row` 画，`work.rs`）。
//!
//! diff 的行照 Puffin（docs/specs/rust-cli/tui-kit.md §4.2「diff」，`shell/tool_view.rs` 的 `diff_row`）：竖线 `   │ `、
//! 4 格的行号、一格、符号（`+ ` ok、`− ` err）与 fg2 的文字，改动的行的底色（`add_bg` / `del_bg`）从符号涂到正文宽的
//! 行尾；上下文行 fg3、没有底色。旧界面的 diff 没有行号，行号的位置留空（§6）；以前配对的行里改动的词再加深一档，Puffin
//! 不按词标，S1 去掉（§6）。
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use serde_json::Value;

use super::Ctx;
use super::look::{self, rail};
use crate::view::ToolKind;

/// 改动前后保留的上下文行数。
const CONTEXT: usize = 3;
/// 行号的格数（Puffin `pad_left(n, 4)`）。
const NUMBER: usize = 4;

/// diff 的一行：符号（`+`、`-`、空格；段之间是 `.`）与文字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatchRow {
    sign: char,
    text: String,
}

fn row(sign: char, text: &str) -> PatchRow {
    PatchRow {
        sign,
        text: text.to_owned(),
    }
}

/// 两段文字的行级 diff：相同的开头与结尾各留 [`CONTEXT`] 行上下文，中间是删除与新增。
fn diff_text(old: &str, new: &str, out: &mut Vec<PatchRow>) {
    let old: Vec<&str> = old.lines().collect();
    let new: Vec<&str> = new.lines().collect();
    let prefix = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let suffix = old[prefix..]
        .iter()
        .rev()
        .zip(new[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    for line in &old[prefix.saturating_sub(CONTEXT)..prefix] {
        out.push(row(' ', line));
    }
    for line in &old[prefix..old.len() - suffix] {
        out.push(row('-', line));
    }
    for line in &new[prefix..new.len() - suffix] {
        out.push(row('+', line));
    }
    for line in old[old.len() - suffix..].iter().take(CONTEXT) {
        out.push(row(' ', line));
    }
}

/// 文件类工具的 diff 行（审批的改动预览也用它）。
pub(crate) fn patch_rows(kind: ToolKind, input: &Value) -> Vec<PatchRow> {
    let text = |key: &str| input[key].as_str().unwrap_or_default();
    let mut out = Vec::new();
    match kind {
        ToolKind::Edit => diff_text(text("old_string"), text("new_string"), &mut out),
        ToolKind::MultiEdit => {
            for (index, edit) in input["edits"].as_array().into_iter().flatten().enumerate() {
                if index > 0 {
                    out.push(row('.', ""));
                }
                let part = |key: &str| edit[key].as_str().unwrap_or_default();
                diff_text(part("old_string"), part("new_string"), &mut out);
            }
        }
        ToolKind::Write => out.extend(text("content").lines().map(|line| row('+', line))),
        ToolKind::NotebookEdit => {
            out.extend(text("new_source").lines().map(|line| row('+', line)));
        }
        ToolKind::Bash
        | ToolKind::Read
        | ToolKind::Grep
        | ToolKind::Glob
        | ToolKind::List
        | ToolKind::WebSearch
        | ToolKind::WebFetch
        | ToolKind::AskUserQuestion
        | ToolKind::ExitPlanMode
        | ToolKind::Agent
        | ToolKind::TodoRead
        | ToolKind::ToolSearch
        | ToolKind::WaitForMcpServers
        | ToolKind::Mcp
        | ToolKind::Other => {}
    }
    out
}

/// git 的统一 diff 正文（第一个 `@@` 起的行，`/diff`，tui-manage.md §4.1）：`@@` 段之间一个 `...`，`\` 开头的
/// 说明行不显示。
pub(crate) fn unified_rows(lines: &[String]) -> Vec<PatchRow> {
    let mut out = Vec::new();
    for line in lines {
        if line.starts_with("@@") {
            if !out.is_empty() {
                out.push(row('.', ""));
            }
            continue;
        }
        match line.chars().next() {
            Some(sign @ ('+' | '-' | ' ')) => out.push(row(sign, &line[1..])),
            None => out.push(row(' ', "")),
            Some(_) => {}
        }
    }
    out
}

/// 多次文件工具调用的 diff 行，调用之间一个 `...`（`/diff` 的回合详情，tui-manage.md §4.2）。
pub(crate) fn edits_rows(edits: &[(String, Value)]) -> Vec<PatchRow> {
    let mut out = Vec::new();
    for (name, input) in edits {
        if !out.is_empty() {
            out.push(row('.', ""));
        }
        out.extend(patch_rows(ToolKind::of(name), input));
    }
    out
}

/// diff 的行（最多 `limit` 行）与没显示的行数，每行照 Puffin 的 `diff_row`（见模块注释）：正文宽 `w = min(宽 − 2, 99)`，
/// 文字截到 `w − 12` 格（竖线 5、行号 4 与一格、符号 2），改动的行用空格补到这一宽度好让底色涂到行尾。段之间的 `...` 在
/// 文字的位置，fg3（Puffin 没有，自拟，tui-kit.md §4.3）。
pub(crate) fn diff(rows: &[PatchRow], ctx: &Ctx<'_>, limit: usize) -> (Vec<StyledLine>, usize) {
    let theme = &ctx.ink.theme;
    let width = look::measure(ctx.width);
    let room = (width as i64 - 5 - 5 - 2).max(1) as usize;
    let lead = || {
        let mut head = rail(ctx.ink);
        head.push(Span::new(" ".repeat(NUMBER), theme.fg3));
        head.push(Span::plain(" "));
        head
    };
    let mut lines = Vec::new();
    for row in rows.iter().take(limit) {
        let (sign, sign_ink, bg) = match row.sign {
            '+' => ("+ ", Style::fg(theme.ok), theme.add_bg),
            '-' => ("− ", Style::fg(theme.err), theme.del_bg),
            '.' => {
                let mut head = lead();
                head.push(Span::new("  ...", theme.fg3));
                lines.push(StyledLine::new(head));
                continue;
            }
            _ => ("  ", theme.fg3, None),
        };
        let text_ink = if row.sign == ' ' {
            theme.fg3
        } else {
            theme.fg2
        };
        let on = |style: Style| bg.map_or(style, |bg| style.with_bg(bg));
        let text = look::clip(&row.text.replace('\t', "    ").replace('\r', ""), room);
        let pad = if bg.is_some() {
            " ".repeat(room.saturating_sub(string_width(&text)))
        } else {
            String::new()
        };
        let mut head = lead();
        head.push(Span::new(sign, on(sign_ink)));
        head.push(Span::new(text + &pad, on(text_ink)));
        lines.push(StyledLine::new(look::cut(&head, width as i64)));
    }
    (lines, rows.len().saturating_sub(limit))
}
