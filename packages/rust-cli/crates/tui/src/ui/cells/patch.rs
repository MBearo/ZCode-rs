// 改写自 apps/zcode-host/crates/tui/src/ui/cells/patch.rs（feat/rs 79b6691c55）：本工程的工具结果没有
// `structuredPatch`，diff 从工具输入算（Edit 的 old_string / new_string，Write 的 content），不带行号。
//! 文件改动的细节：`Added N lines, removed M lines` 或 `Wrote N lines to 路径`，下面是带 `+`/`-` 的
//! diff。新增与删除行整行着底色，配对的行里改动的词再加深一档。紧凑模式最多显示
//! [`COMPACT_LIMIT`] 行 diff，详细模式全部显示。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use serde_json::Value;

use super::words;
use super::{Ctx, DETAIL, INDENT, bold, dim, more_lines};
use crate::ui::palette::ColorLevel;
use crate::ui::text::truncate_line;
use crate::view::ToolRow;

/// 紧凑模式显示的 diff 行数。
const COMPACT_LIMIT: usize = 20;
/// 改动前后保留的上下文行数。
const CONTEXT: usize = 3;

/// 新增 / 删除行的样式：整行底色、符号、文字与改动的词。
struct Kind {
    line: Style,
    sign: Style,
    text: Style,
    word: Style,
}

fn styles(ctx: &Ctx<'_>, added: bool) -> Kind {
    let fg = if added { Color::Green } else { Color::Red };
    let pick = |dark: (u8, u8, u8), light: (u8, u8, u8), dark256: u8, light256: u8| match ctx
        .palette
        .level
    {
        ColorLevel::TrueColor => Some(if ctx.palette.light {
            Color::Rgb(light.0, light.1, light.2)
        } else {
            Color::Rgb(dark.0, dark.1, dark.2)
        }),
        ColorLevel::Ansi256 => Some(Color::Indexed(if ctx.palette.light {
            light256
        } else {
            dark256
        })),
        ColorLevel::Ansi16 => None,
    };
    let (line_bg, word_bg) = if added {
        (
            pick((0x21, 0x3a, 0x2b), (0xda, 0xfb, 0xe1), 22, 194),
            pick((0x2e, 0x60, 0x3f), (0xac, 0xee, 0xbb), 28, 157),
        )
    } else {
        (
            pick((0x4a, 0x22, 0x1d), (0xff, 0xeb, 0xe9), 52, 224),
            pick((0x7a, 0x33, 0x2b), (0xff, 0xc1, 0xbd), 88, 217),
        )
    };
    let (Some(bg), Some(word_bg)) = (line_bg, word_bg) else {
        // 16 色终端不画底色：符号与文字上色，改动的词加粗。
        let style = Style::default().fg(fg);
        return Kind {
            line: Style::default(),
            sign: style,
            text: style,
            word: style.add_modifier(Modifier::BOLD),
        };
    };
    // 浅色背景：文字保持默认前景色，只给符号上色。
    let text = if ctx.palette.light {
        Style::default()
    } else {
        Style::default().fg(fg)
    };
    Kind {
        line: Style::default().bg(bg),
        sign: Style::default().fg(fg),
        text,
        word: text.bg(word_bg),
    }
}

/// diff 的一行：符号（`+`、`-`、空格；段之间是 `.`）与文字。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PatchRow {
    sign: char,
    text: String,
    /// 改动的词（字节范围）；没有配对或改动太多时为空。
    words: Vec<(usize, usize)>,
}

fn row(sign: char, text: &str) -> PatchRow {
    PatchRow {
        sign,
        text: text.to_owned(),
        words: Vec::new(),
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
pub(crate) fn patch_rows(name: &str, input: &Value) -> Vec<PatchRow> {
    let text = |key: &str| input[key].as_str().unwrap_or_default();
    let mut out = Vec::new();
    match name {
        "Edit" => diff_text(text("old_string"), text("new_string"), &mut out),
        "MultiEdit" => {
            for (index, edit) in input["edits"].as_array().into_iter().flatten().enumerate() {
                if index > 0 {
                    out.push(row('.', ""));
                }
                let part = |key: &str| edit[key].as_str().unwrap_or_default();
                diff_text(part("old_string"), part("new_string"), &mut out);
            }
        }
        "Write" => out.extend(text("content").lines().map(|line| row('+', line))),
        "NotebookEdit" => out.extend(text("new_source").lines().map(|line| row('+', line))),
        _ => {}
    }
    pair(&mut out);
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
    pair(&mut out);
    out
}

/// 多次文件工具调用的 diff 行，调用之间一个 `...`（`/diff` 的回合详情，tui-manage.md §4.2）。
pub(crate) fn edits_rows(edits: &[(String, Value)]) -> Vec<PatchRow> {
    let mut out = Vec::new();
    for (name, input) in edits {
        if !out.is_empty() {
            out.push(row('.', ""));
        }
        out.extend(patch_rows(name, input));
    }
    out
}

/// 一段连续的删除行后紧跟一段新增行时，按顺序逐对比较，记下改动的词。
fn pair(rows: &mut [PatchRow]) {
    let mut index = 0;
    while index < rows.len() {
        let start = index;
        while index < rows.len() && rows[index].sign == '-' {
            index += 1;
        }
        let middle = index;
        while index < rows.len() && rows[index].sign == '+' {
            index += 1;
        }
        let count = (middle - start).min(index - middle);
        for offset in 0..count {
            let (old, new) = (start + offset, middle + offset);
            if let Some((old_words, new_words)) =
                super::diff::changed(&rows[old].text, &rows[new].text)
            {
                rows[old].words = old_words;
                rows[new].words = new_words;
            }
        }
        if index == start {
            index += 1;
        }
    }
}

/// 文字按改动的词切成片段。
fn text_spans(row: &PatchRow, kind: &Kind) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut at = 0;
    for (start, end) in &row.words {
        if *start > at {
            spans.push(Span::styled(row.text[at..*start].to_owned(), kind.text));
        }
        spans.push(Span::styled(row.text[*start..*end].to_owned(), kind.word));
        at = *end;
    }
    if at < row.text.len() || spans.is_empty() {
        spans.push(Span::styled(row.text[at..].to_owned(), kind.text));
    }
    spans
}

/// 计数行：（前文，粗体的数字，后文）若干段。
fn counted(parts: Vec<words::Counted>) -> Line<'static> {
    let mut spans = vec![Span::styled(DETAIL, dim())];
    for (before, count, after) in parts {
        spans.push(Span::raw(before));
        spans.push(Span::styled(count, bold()));
        spans.push(Span::raw(after));
    }
    Line::from(spans)
}

pub fn details(tool: &ToolRow, ctx: &Ctx<'_>) -> Vec<Line<'static>> {
    let rows = patch_rows(&tool.name, &tool.input);
    let path = ["file_path", "notebook_path", "path"]
        .iter()
        .find_map(|key| tool.input[*key].as_str())
        .map(|path| super::shown_path(path, tool, ctx))
        .unwrap_or_default();
    let additions = rows.iter().filter(|row| row.sign == '+').count() as u64;
    let deletions = rows.iter().filter(|row| row.sign == '-').count() as u64;
    let created = tool.name == "Write" && tool.display["type"] == "create";
    let summary = if created {
        vec![words::wrote(ctx.lang, additions, &path)]
    } else {
        words::changed(ctx.lang, additions, deletions)
    };
    let mut lines = vec![truncate_line(counted(summary), ctx.width)];
    let limit = if ctx.expanded {
        usize::MAX
    } else {
        COMPACT_LIMIT
    };
    let (diff, hidden) = diff_lines(&rows, ctx, limit);
    lines.extend(diff);
    if hidden > 0 {
        lines.push(more_lines(hidden, ctx));
    }
    lines
}

/// diff 的行（最多 `limit` 行）与没显示的行数。
pub(crate) fn diff_lines(
    rows: &[PatchRow],
    ctx: &Ctx<'_>,
    limit: usize,
) -> (Vec<Line<'static>>, usize) {
    let (added, removed) = (styles(ctx, true), styles(ctx, false));
    // 缩进不着底色（整行底色从符号开始）。
    let indent = Span::styled(INDENT, Style::default().bg(Color::Reset));
    let mut lines = Vec::new();
    for row in rows.iter().take(limit) {
        let line = match row.sign {
            '.' => Line::from(vec![indent.clone(), Span::styled("...", dim())]),
            '+' | '-' => {
                let kind = if row.sign == '+' { &added } else { &removed };
                let mut spans = vec![
                    indent.clone(),
                    Span::styled(row.sign.to_string(), kind.sign),
                    Span::raw(" "),
                ];
                spans.extend(text_spans(row, kind));
                Line::from(spans).style(kind.line)
            }
            _ => Line::from(vec![
                indent.clone(),
                Span::styled(format!("  {}", row.text), dim()),
            ]),
        };
        lines.push(truncate_line(line, ctx.width));
    }
    (lines, rows.len().saturating_sub(limit))
}
