//! 你的回合的板（docs/puffin-tui.md §4.2「发出的消息」「粘贴」「提及（@）」；docs/specs/rust-cli/tui-kit.md §4.4）：板上满墨的
//! 文字（plain 模式保留 `›`、文字改成安静的），开头的命令名是 brand，提及按种类上墨；高过 [`SLAB_ROWS`] 时只显示开头
//! [`SLAB_HEAD`] 行与末尾 [`SLAB_TAIL`] 行，中间一行折叠说明，除非打开（^O）。入参是已经分好的片段（[`Piece`]）与界面画好
//! 的文件行；怎样认出提及、附件怎样画留在界面。
//! 移植自 Puffin 的 shell/view/entries.rs 的 `user_turn` 的排版部分。
use crate::app::element::{
    Clip, ClipFocus, Element, Wrap, br, measure, more_lines, slab, stack, wrap_text,
};
use crate::render::ansi::RESET;
use crate::render::color::Color;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::fit::{MEASURE_MARGIN, MEASURE_MAX};

/// 用户行在板上至多这么多行（收起时）。
pub const SLAB_ROWS: usize = 12;
/// 收起时显示开头的这么多行……
pub const SLAB_HEAD: usize = 6;
/// ……与末尾的这么多行。
pub const SLAB_TAIL: usize = 3;
/// 提示符：`›`（在一屏的骨架旁边）。
pub use crate::app::frame::PROMPT_ARROW;
/// 第一行之后的行从文字下面开始：过了边距、`›` 与一个空格。
pub const INDENT: &str = "   ";

/// 板上文字的一段。
#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    /// 平常的字。
    Text(String),
    /// 提及：`@标签` 或 `$技能`，与它的种类（`skill`、`file`、`work`，其余是子代理）。
    Mention(String, &'static str),
    /// 已经上好墨的一段（图片的键帽）：空格换成不换行的，不拆到两行。
    Cap(Vec<Span>),
}

/// 提及的种类的墨色。
pub fn mention_ink(theme: &Theme, kind: &str) -> Color {
    match kind {
        "skill" => theme.skill,
        "file" => theme.file,
        "work" => theme.work,
        _ => theme.agent,
    }
}

/// 板的第一行的开头：` › `，brand 粗体。
pub fn chevron(theme: &Theme) -> Vec<Span> {
    vec![
        Span::new(" ", Style::DEFAULT),
        Span::new(PROMPT_ARROW, Style::fg(theme.brand).with_attr(attr::BOLD)),
        Span::new(" ", Style::DEFAULT),
    ]
}

/// 板上其余行的开头。
pub fn indent() -> Vec<Span> {
    vec![Span::new(INDENT, Style::DEFAULT)]
}

/// 板上的文字（带 SGR）：开头的命令名 `command`（没有时为 ""）是 brand，其后是 `pieces`。有板时文字是满墨的；plain 模式
/// 是 fg2，每个上了墨的段之后接回 fg2。
pub fn words(theme: &Theme, command: &str, pieces: &[Piece]) -> String {
    let seq = |st: Style| st.set_sequence();
    let named = if command.is_empty() {
        String::new()
    } else {
        format!("{}{command}{RESET}", seq(Style::fg(theme.brand)))
    };
    let after = if theme.panel.is_some() {
        String::new()
    } else {
        seq(theme.fg2)
    };
    let rest: String = pieces
        .iter()
        .map(|p| match p {
            Piece::Text(t) => t.clone(),
            Piece::Mention(t, kind) => {
                format!(
                    "{}{t}{RESET}{after}",
                    seq(Style::fg(mention_ink(theme, kind)))
                )
            }
            Piece::Cap(spans) => {
                let mut s: String = spans
                    .iter()
                    .map(|sp| format!("{}{}{RESET}", seq(sp.style), sp.text.replace(' ', "\u{a0}")))
                    .collect();
                s.push_str(&after);
                s
            }
        })
        .collect();
    if theme.panel.is_some() {
        format!("{named}{rest}")
    } else {
        format!("{named}{}{rest}{RESET}", seq(theme.fg2))
    }
}

/// 板：`words` 折在 ` › ` 之后（`words` 为 `None` 时没有文字行），然后是文件行；收起时按 6 + 3 行剪，有板色时铺在板上。
/// `pictures` 是打开时画在板下面的图（空行隔开）。
pub fn turn(
    theme: &Theme,
    words: Option<String>,
    files: Vec<Element>,
    open: bool,
    pictures: Vec<Element>,
) -> Element {
    turn_said(theme, words, files, open, pictures, more_lines)
}

/// [`turn`]，剪去的标记说 `more` 的话（别的界面语言）。
pub fn turn_said(
    theme: &Theme,
    words: Option<String>,
    files: Vec<Element>,
    open: bool,
    pictures: Vec<Element>,
    more: fn(i64) -> String,
) -> Element {
    let chevron = format!(
        " {}{PROMPT_ARROW}{RESET} ",
        Style::fg(theme.brand).with_attr(attr::BOLD).set_sequence()
    );
    let mut rows = Vec::new();
    if let Some(w) = words {
        rows.push(wrap_text(chevron, "   ", w, Wrap::Word));
    }
    rows.extend(files);
    let body = stack(rows);
    let clamped = if open {
        body
    } else {
        Clip::new(
            body,
            SLAB_ROWS,
            ClipFocus::Ends {
                head: SLAB_HEAD,
                tail: SLAB_TAIL,
            },
        )
        .with_lead(indent())
        .with_ink(theme.fg3)
        .with_more(more)
        .into()
    };
    let turn = match theme.panel {
        Some(panel) => slab(measure(clamped, MEASURE_MAX, MEASURE_MARGIN), panel),
        None => measure(clamped, MEASURE_MAX, MEASURE_MARGIN),
    };
    if pictures.is_empty() {
        turn
    } else {
        let mut all = vec![turn, br()];
        all.extend(pictures);
        stack(all)
    }
}
