//! transcript 里几种单行或简单的条目（docs/puffin-tui.md §4.2；docs/specs/rust-cli/tui-kit.md §4.4）：压缩行（写得像名为
//! `Compact` 的调用，`52.3k → 7.9k tokens`）、只有名字与摘要的工具行、从第 3 列起的安静的一行、回合停下的 `Stopped.`，与
//! ^O 打开时单独画的思考。入参是朴素的数据。
//! 移植自 Puffin 的 shell/view/entries.rs 的 `compact_row`、`tool_row`、`quiet_line`、`thought` 与
//! shell/view.rs 的 `stopped` 的那一行。
use crate::app::element::{Element, Wrap, caret_at, single_line, stack, text, wrap_styled};
use crate::markdown::render::INDENT;
use crate::render::ansi::RESET;
use crate::render::{Span, Style, attr};
use crate::theme::ease::Ink;
use crate::theme::theme::Theme;

use super::fit::{self, measured};
use super::pen::{self, Pen};
use super::playhead::{self, Playhead};

/// 一次压缩的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compaction {
    Running,
    Success,
    Noop,
    Cancelled,
    Failed,
}

/// 压缩行与停下的那一行的字。别的界面语言给译文；铺法不变。
pub struct EntryWords {
    /// 在跑的压缩的摘要。
    pub summarising: &'static str,
    /// 没有可压缩的。
    pub nothing: &'static str,
    /// 压缩被停下了。
    pub cancelled: &'static str,
    /// 压缩失败了。
    pub failed: &'static str,
    /// 回合停下了。
    pub stopped: &'static str,
    /// 单独画的思考（^O）的标题：流式时与结束后。
    pub thinking: &'static str,
    pub thought: &'static str,
}

/// Puffin 的字。
pub const ENTRY_WORDS: EntryWords = EntryWords {
    summarising: "summarising the conversation",
    nothing: "nothing to compact",
    cancelled: "stopped",
    failed: "failed",
    stopped: "Stopped.",
    thinking: "Thinking",
    thought: "Thought",
};

/// 一次压缩，写得像一个名为 Compact 的工具调用（§4.2）：成功时说 `52.3k → 7.9k tokens`（两个数都知道时）。
pub fn compact_row(
    theme: &Theme,
    status: Compaction,
    before: Option<u64>,
    after: Option<u64>,
) -> Element {
    compact_row_said(theme, status, before, after, &ENTRY_WORDS)
}

/// [`compact_row`]，字是 `w` 的（别的界面语言）。
pub fn compact_row_said(
    theme: &Theme,
    status: Compaction,
    before: Option<u64>,
    after: Option<u64>,
    w: &EntryWords,
) -> Element {
    let saved = match (before, after) {
        (Some(b), Some(a)) => Some(format!(
            "{} → {} tokens",
            fit::tokens(b as f64),
            fit::tokens(a as f64)
        )),
        _ => None,
    };
    let (status, summary) = match status {
        Compaction::Running => ("running", w.summarising.to_owned()),
        Compaction::Success => ("done", saved.unwrap_or_default()),
        Compaction::Noop => ("cancelled", w.nothing.to_owned()),
        Compaction::Cancelled => ("cancelled", w.cancelled.to_owned()),
        Compaction::Failed => ("error", w.failed.to_owned()),
    };
    tool_row(theme, status, "Compact", &summary, None)
}

/// 一个工具调用：标记、名称、安静的摘要；一行，用 `…` 截断。`status` 是 `running`、`queued`、`asking`、`done`、`error`
/// 或 `cancelled`。等你的行的 `●` 是琥珀色，或淡入时的 `asking`（§4.2 请求）。
pub fn tool_row(
    theme: &Theme,
    status: &str,
    name: &str,
    summary: &str,
    asking: Option<Style>,
) -> Element {
    let (mark, name_style) = match status {
        "asking" => (
            Span::new("●", asking.unwrap_or(Style::fg(theme.code))),
            theme.fg2,
        ),
        "done" => (Span::new("✓", Style::fg(theme.ok)), theme.fg2),
        "error" => (Span::new("✗", Style::fg(theme.err)), theme.fg2),
        "cancelled" => (Span::new("–", theme.fg3), theme.fg3),
        // 在跑：不动。只有工作光标动（§4.2）。
        _ => (Span::new("○", theme.fg3), theme.fg2),
    };
    let mut spans = vec![
        Span::new(" ", Style::DEFAULT),
        mark,
        Span::new(" ", Style::DEFAULT),
        Span::new(name.to_owned(), name_style.with_attr(attr::BOLD)),
    ];
    if !summary.is_empty() {
        spans.push(Span::new("  ", Style::DEFAULT));
        spans.push(Span::new(summary.to_owned(), theme.fg3));
    }
    single_line(spans, false)
}

/// 一行安静的字，从第 3 列起（`Queued.` 之类）。
pub fn quiet_line(theme: &Theme, words: &str) -> Element {
    text(format!(
        "{INDENT}{}{words}{RESET}",
        theme.fg3.set_sequence()
    ))
}

/// 回合停下了：fg3 斜体的 `Stopped.`；`goal_paused` 时接着说目标暂停了、`/goal resume` 接着做（§4.2「目标」的「停下」）。
pub fn stopped_line(theme: &Theme, goal_paused: bool) -> Element {
    stopped_line_said(theme, goal_paused, &ENTRY_WORDS)
}

/// [`stopped_line`]，`Stopped.` 是 `w` 的（别的界面语言）；目标暂停的那句只有 Puffin 的字。
pub fn stopped_line_said(theme: &Theme, goal_paused: bool, w: &EntryWords) -> Element {
    let quiet = theme.fg3.with_attr(attr::ITALIC).set_sequence();
    let goal = if goal_paused {
        format!(
            " The goal is paused; {RESET}{}/goal resume{RESET}{quiet} picks it up.",
            theme.fg2.set_sequence()
        )
    } else {
        String::new()
    };
    text(format!("{INDENT}{quiet}{}{goal}{RESET}", w.stopped))
}

/// 给格子的纯文字：tab 与回车会弄乱它。
fn clean(s: &str) -> String {
    s.replace('\r', "").replace('\t', "    ")
}

/// 单独画的思考，如 ^O 打开它（§4.2「思考」）：一行标题，流式时 `Thinking`、结束后 `Thought · 6s`（`duration_ms`），下面是
/// 到此显示的文字，纯文字，在竖线后面。给了笔时它以 fg3 骑在显示的末尾；最新的词落定到 fg3（§4.2「笔」）。`stopped` 是回合
/// 停下了：字不再落定。
#[allow(clippy::too_many_arguments)]
pub fn thought(
    theme: &Theme,
    raw: &str,
    ended: bool,
    duration_ms: Option<u64>,
    pen: Option<&Pen>,
    head: Option<&Playhead>,
    now: i64,
    stopped: bool,
) -> Element {
    thought_said(
        theme,
        raw,
        ended,
        duration_ms,
        pen,
        head,
        now,
        stopped,
        &ENTRY_WORDS,
    )
}

/// 同 [`thought`]，标题的字是 `w`。
#[allow(clippy::too_many_arguments)]
pub fn thought_said(
    theme: &Theme,
    raw: &str,
    ended: bool,
    duration_ms: Option<u64>,
    pen: Option<&Pen>,
    head: Option<&Playhead>,
    now: i64,
    stopped: bool,
    w: &EntryWords,
) -> Element {
    let shown = head.map_or(raw.len(), |h| h.shown.min(raw.len()));
    let indent = Span::new(INDENT, Style::DEFAULT);
    let label = Span::new(
        if ended { w.thought } else { w.thinking },
        theme.fg3.with_attr(attr::ITALIC),
    );
    let mut header = vec![indent, label];
    if ended && let Some(ms) = duration_ms {
        header.push(Span::new(" · ", theme.line));
        header.push(Span::new(fit::thought_time(ms as f64), theme.fg3));
    }
    let nib = match (pen, head) {
        (Some(p), Some(h)) => Some(p.nib(Ink::Thought, h, now)),
        _ => None,
    };
    let rail = vec![
        Span::new(INDENT, Style::DEFAULT),
        Span::new("│", theme.line),
        Span::new(" ", Style::DEFAULT),
    ];
    let hidden = playhead::word_end(raw, shown) - shown;
    let paras: Vec<String> = clean(&raw[..shown])
        .split('\n')
        .map(str::to_owned)
        .collect();
    // 还在写的思考的字标成 agent 写的，最新的词落定到 fg3；还没显示的字母占着位置但不画（§4.2「流式节奏」）。
    let ink = if head.is_some() {
        theme.fg3.with_attr(attr::WRITTEN)
    } else {
        theme.fg3
    };
    let last = paras.len() - 1;
    let mut rows = vec![single_line(header, false)];
    for (i, p) in paras.into_iter().enumerate() {
        let is_last = i == last;
        if p.trim_matches(|c: char| c <= ' ').is_empty() && (!is_last || hidden == 0) {
            rows.push(single_line(rail[..2].to_vec(), false));
        } else {
            let mut spans = vec![Span::new(p, ink)];
            if is_last && hidden > 0 {
                // 还没显示的字母：不折行的空格占着它们的位置（按码点，Scala 按 UTF-16 单元）。
                let n = raw[shown..shown + hidden].chars().count();
                spans.push(Span::new("\u{a0}".repeat(n), Style::DEFAULT));
            }
            rows.push(wrap_styled(rail.clone(), rail.clone(), spans, Wrap::Word));
        }
    }
    let laid = stack(rows);
    let written = match head.filter(|_| !stopped) {
        Some(h) => pen::settled(theme, laid, raw, h, now),
        None => laid,
    };
    let caret = nib.or_else(|| pen.map(|p| p.rest(Ink::Thought)));
    let body = match caret {
        Some(c) => caret_at(written, vec![c]),
        None => written,
    };
    measured(body)
}
