//! 通知的行（docs/puffin-tui.md §4.2「通知」；docs/specs/rust-cli/tui-kit.md §4.4）：开启后台结果那一轮的一行——记号、fg2
//! 粗体的种类词、标题与发生了什么——与它下面轨道上的话（至多四行，其余说成 `… N more lines`）。入参是中性的 [`Notice`]。
//! 移植自 Puffin 的 shell/notices.rs 的画法（Notices.scala 第 1–179 行）；响不响铃留在界面。
//!
//! 四种：系统提示（`• Notice`，警告是 code 色的 `● Warning`，文字在轨道上）；后台的命令或子代理回来了（`• Command|Subagent
//! 标题 finished`）；同伴的消息（`• Peer 名字 says`，消息在轨道上）；你发出的消息的回执（`标记 Sent 名字 结果`）。
use crate::app::element::{
    Clip, ClipFocus, Element, Wrap, by_width, more_lines, single_line, stack, wrap_text,
};
use crate::markdown::render::INDENT;
use crate::render::ansi::RESET;
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::fit::{clip, measured};

/// 行下面先显示这么多行，其余说成 `… N more lines`。
const BODY_ROWS: usize = 4;

/// 回来的是哪种后台工作。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Work {
    Command,
    Subagent,
}

/// 你发出的消息的回执。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Receipt {
    Held,
    Denied,
    Expired,
    Delivered,
    Refused,
}

/// 一条通知，作为中性的数据。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Notice<'a> {
    /// 只供显示的系统提示；`warning` 时是警告。
    System { warning: bool, text: &'a str },
    /// 后台的工作回来了。
    Finished { work: Work, title: &'a str },
    /// 同伴发来的消息。
    Peer { name: &'a str, body: &'a str },
    /// 你发给 `name` 的消息的回执。
    Sent { name: &'a str, receipt: Receipt },
}

/// 通知里的字：种类词、发生了什么、回执与剪去的标记。别的界面语言给译文；铺法不变。
pub struct NoticeWords {
    pub notice: &'static str,
    pub warning: &'static str,
    pub command: &'static str,
    pub subagent: &'static str,
    pub peer: &'static str,
    pub sent: &'static str,
    pub finished: &'static str,
    pub says: &'static str,
    pub delivered: &'static str,
    pub held: &'static str,
    pub denied: &'static str,
    pub refused: &'static str,
    pub expired: &'static str,
    pub more: fn(i64) -> String,
}

/// Puffin 的字。
pub const NOTICE_WORDS: NoticeWords = NoticeWords {
    notice: "Notice",
    warning: "Warning",
    command: "Command",
    subagent: "Subagent",
    peer: "Peer",
    sent: "Sent",
    finished: "finished",
    says: "says",
    delivered: "delivered",
    held: "held for approval",
    denied: "denied",
    refused: "refused",
    expired: "expired",
    more: more_lines,
};

/// 行与它下面回来的话。`p` 是它的记号与种类从背景淡入到哪里了：0 刚到，1 落定。
pub fn entry(theme: &Theme, n: &Notice<'_>, p: f64) -> Element {
    entry_said(theme, n, p, &NOTICE_WORDS)
}

/// [`entry`]，字是 `w` 的（别的界面语言）。
pub fn entry_said(theme: &Theme, n: &Notice<'_>, p: f64, w: &NoticeWords) -> Element {
    let shown = match *n {
        Notice::System { warning, text } => Shown {
            glyph: if warning { "●" } else { "•" },
            ink: if warning {
                Style::fg(theme.code)
            } else {
                theme.fg3
            },
            kind: if warning { w.warning } else { w.notice },
            title: String::new(),
            what: Vec::new(),
            body: lines(text),
        },
        Notice::Finished { work, title } => Shown {
            glyph: "•",
            ink: theme.fg3,
            kind: match work {
                Work::Command => w.command,
                Work::Subagent => w.subagent,
            },
            title: title.to_owned(),
            what: vec![Span::new(w.finished, theme.fg3)],
            body: Vec::new(),
        },
        Notice::Peer { name, body } => Shown {
            glyph: "•",
            ink: theme.fg3,
            kind: w.peer,
            title: name.to_owned(),
            what: vec![Span::new(w.says, theme.fg3)],
            body: lines(body),
        },
        Notice::Sent { name, receipt } => {
            let (glyph, ink, word) = match receipt {
                Receipt::Delivered => ("✓", Style::fg(theme.ok), w.delivered),
                Receipt::Held => ("○", theme.fg3, w.held),
                Receipt::Denied => ("✗", Style::fg(theme.err), w.denied),
                Receipt::Refused => ("✗", Style::fg(theme.err), w.refused),
                Receipt::Expired => ("–", theme.fg3, w.expired),
            };
            Shown {
                glyph,
                ink,
                kind: w.sent,
                title: name.to_owned(),
                what: vec![Span::new(word, ink)],
                body: Vec::new(),
            }
        }
    };
    let kind_ink = theme.fg2.with_attr(attr::BOLD);
    let head = row(
        theme,
        Span::new(shown.glyph, faded(theme, shown.ink, p)),
        Span::new(shown.kind, faded(theme, kind_ink, p)),
        shown.title,
        shown.what,
    );
    if shown.body.is_empty() {
        return head;
    }
    let rail = rail(theme);
    let seq = theme.fg2.set_sequence();
    let rows = shown
        .body
        .into_iter()
        .map(|l| {
            wrap_text(
                rail.clone(),
                rail.clone(),
                format!("{seq}{l}{RESET}"),
                Wrap::Word,
            )
        })
        .collect::<Vec<_>>();
    let clipped = Clip::new(stack(rows), BODY_ROWS, ClipFocus::Head)
        .with_lead(rail_lead(theme))
        .with_ink(theme.fg3)
        .with_more(w.more);
    stack([head, measured(clipped.into())])
}

/// 一行要画的：记号与它的墨色、种类、标题、发生了什么、下面的行。
struct Shown {
    glyph: &'static str,
    ink: Style,
    kind: &'static str,
    title: String,
    what: Vec<Span>,
    body: Vec<String>,
}

/// 文字的非空行，去掉 `\r`，tab 换成四个空格。
fn lines(text: &str) -> Vec<String> {
    text.replace('\r', "")
        .replace('\t', "    ")
        .split('\n')
        .filter(|l| !l.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// 一行：记号、种类、标题与发生了什么。放不下时先用 `…` 截标题，再去掉它；发生了什么保持完整。
fn row(theme: &Theme, glyph: Span, kind: Span, title: String, what: Vec<Span>) -> Element {
    let lead = vec![
        Span::new(" ", Style::DEFAULT),
        glyph,
        Span::new(" ", Style::DEFAULT),
        kind,
    ];
    // 没有可说的结果时行尾不留两个空格。
    let tail: Vec<Span> = if what.is_empty() {
        Vec::new()
    } else {
        std::iter::once(Span::new("  ", Style::DEFAULT))
            .chain(what)
            .collect()
    };
    let fixed: usize = lead
        .iter()
        .chain(tail.iter())
        .map(|sp| string_width(&sp.text))
        .sum();
    let title_ink = theme.fg3;
    by_width(move |w| {
        let room = w as i64 - fixed as i64 - 2;
        let shown = if (string_width(&title) as i64) <= room {
            title.clone()
        } else if room >= 4 {
            cut(&title, room as usize)
        } else {
            String::new()
        };
        let mut spans = lead.clone();
        if !shown.is_empty() {
            spans.push(Span::new("  ", Style::DEFAULT));
            spans.push(Span::new(shown, title_ink));
        }
        spans.extend(tail.iter().cloned());
        single_line(spans, false)
    })
}

/// `text` 截到 `cols` 格，以 `…` 结尾，前面不留空格。
pub fn cut(text: &str, cols: usize) -> String {
    let clipped = clip(text, cols);
    let body = clipped.strip_suffix('…').unwrap_or(&clipped);
    format!("{}…", body.trim_end())
}

/// `st` 从背景淡入到 `p` 处的样子（plain 模式立即是它）。
fn faded(theme: &Theme, st: Style, p: f64) -> Style {
    if p >= 1.0 {
        st
    } else {
        theme
            .rise(st.fg_color(), p, None)
            .map_or(st, |c| st.with_fg(c))
    }
}

/// `   │ `：行下面的轨道（带 SGR 的前缀）。
fn rail(theme: &Theme) -> String {
    format!("{INDENT}{}│{RESET} ", theme.line.set_sequence())
}

fn rail_lead(theme: &Theme) -> Vec<Span> {
    vec![
        Span::new(INDENT, Style::DEFAULT),
        Span::new("│", theme.line),
        Span::new(" ", Style::DEFAULT),
    ]
}
