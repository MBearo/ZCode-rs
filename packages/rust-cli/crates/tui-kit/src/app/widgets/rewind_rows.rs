//! 回退的行（docs/puffin-tui.md §4.2「回退（/rewind）」；docs/specs/rust-cli/tui-kit.md §4.4）：切线、留在后面的变暗、挑选时
//! 输入框上方的 Also 与 Files、fork 出的会话的痕迹行、点的列表（年龄、消息的第一行、选中的点的细节）。纯的；入参是中性的
//! [`Point`]。
//! 移植自 Puffin 的 shell/rewind/words.rs（`object Rewind` 除去读条目的 `also`）。
use crate::app::element::{Element, single_line};
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Color, Span, Style, StyledLine, attr};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::fit::{pad, pad_left};
use super::lists::{found, picker_window, row};
use super::when::{self, AGE_WIDTH};

/// 视图在切线上面留这么多行同行的东西。
pub const ABOVE: usize = 4;
/// 切线的规则从不比这短。
pub const RULE_MIN: usize = 6;
/// Also 与 Files 的标签补到这么宽。
pub const LABEL_WIDTH: usize = 5;

/// ⏎ 怎样回去：在上面的回答上 fork；回到开头（新会话）；回不去。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointFrom {
    Start,
    Answer,
    None,
}

/// 回退的一个点：你的一条消息。`at` 是它出现的时刻（epoch ms）；`added` / `removed` 是它之后改的行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Point {
    pub at: u64,
    pub from: PointFrom,
    pub added: u64,
    pub removed: u64,
}

/// 点列表的过滤词与选中的行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct List<'a> {
    pub query: &'a str,
    pub choice: usize,
}

pub fn ordinal(n: usize) -> String {
    let suffix = if (11..=13).contains(&(n % 100)) {
        "th"
    } else {
        match n % 10 {
            1 => "st",
            2 => "nd",
            3 => "rd",
            _ => "th",
        }
    };
    format!("{n}{suffix}")
}

/// 点 `k` 回到哪里。
pub fn where_(points: &[Point], k: usize) -> String {
    where_said(points, k, &REWIND_WORDS)
}

/// 留在后面的回合：从 `k` 起每个点一个。
pub fn behind(points: &[Point], k: usize) -> usize {
    points.len() - k
}

fn turns(n: usize) -> String {
    if n == 1 {
        "the turn below".to_owned()
    } else {
        format!("the {n} turns below")
    }
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|s| string_width(&s.text)).sum()
}

/// 切线与 Also / Files 两行的字（英文是 Puffin 的原文 [`REWIND_WORDS`]；别的界面语言给译文，铺法不变）。
pub struct RewindWords {
    /// 点回到哪里：回到开头、最后一条消息之前、第 n 条之前（`n` 从 1 起，`last` 是最后一个）。
    pub start: &'static str,
    pub before_last: &'static str,
    pub before: fn(usize) -> String,
    /// `Back to …`、`Can't go back to …`。
    pub back: fn(&str) -> String,
    pub cant: fn(&str) -> String,
    pub unfinished: &'static str,
    pub unfinished_short: &'static str,
    /// 留在后面的回合（`all`：回到开头），连同 `, in /resume`。
    pub stay: fn(usize, bool) -> String,
    pub kept: &'static str,
    pub also: &'static str,
    pub files: &'static str,
    pub stay_files: &'static str,
    /// `the 2 turns below changed `（后面接 `+a −d`）。
    pub changed: fn(usize) -> String,
    /// 痕迹行：`Rewound here`，`“原标题” is in /resume, as it was`。
    pub rewound: &'static str,
    pub as_it_was: fn(&str) -> String,
}

fn en_before(n: usize) -> String {
    format!("before your {} message", ordinal(n))
}

fn en_stay(n: usize, all: bool) -> String {
    let stay = if all && n > 1 {
        format!("all {n} turns stay behind")
    } else if n == 1 {
        "the turn below stays behind".to_owned()
    } else {
        format!("{} stay behind", turns(n))
    };
    format!("{stay}, in /resume")
}

fn en_changed(n: usize) -> String {
    format!("{} changed ", turns(n))
}

/// Puffin 的英文。
pub const REWIND_WORDS: RewindWords = RewindWords {
    start: "the start",
    before_last: "before your last message",
    before: en_before,
    back: |w| format!("Back to {w}"),
    cant: |w| format!("Can't go back to {w}"),
    unfinished: "the turn above did not finish",
    unfinished_short: "turn above unfinished",
    stay: en_stay,
    kept: "kept in /resume",
    also: "Also",
    files: "Files",
    stay_files: "stay as they are now",
    changed: en_changed,
    rewound: "Rewound here",
    as_it_was: |t| format!("“{t}” is in /resume, as it was"),
};

fn where_said(points: &[Point], k: usize, w: &RewindWords) -> String {
    if points[k].from == PointFrom::Start {
        w.start.to_owned()
    } else if k + 1 == points.len() {
        w.before_last.to_owned()
    } else {
        (w.before)(k + 1)
    }
}

/// 宽 `width` 的切线：`↶`、标签、规则与说明；变窄时说明先变短，再去掉。
pub fn cut_line(theme: &Theme, points: &[Point], k: usize, width: usize) -> StyledLine {
    cut_line_said(theme, points, k, width, &REWIND_WORDS)
}

/// 同 [`cut_line`]，字是 `w`。
pub fn cut_line_said(
    theme: &Theme,
    points: &[Point],
    k: usize,
    width: usize,
    w: &RewindWords,
) -> StyledLine {
    let p = &points[k];
    let blocked = p.from == PointFrom::None;
    let n = behind(points, k);
    let at = where_said(points, k, w);
    let (label, note, short) = if blocked {
        ((w.cant)(&at), w.unfinished.to_owned(), w.unfinished_short)
    } else {
        (
            (w.back)(&at),
            (w.stay)(n, p.from == PointFrom::Start),
            w.kept,
        )
    };
    // 整个 TUI 只有这一条满宽的彩色线；回不去的点换成 line 色，位置照样清楚。
    let ink = if blocked {
        theme.line
    } else {
        Style::fg(if theme.rich {
            mixed(theme, theme.brand, 0.3)
        } else {
            theme.brand
        })
    };
    let head = if blocked {
        theme.fg3.with_attr(attr::BOLD)
    } else {
        Style::new(theme.brand, Color::Default, attr::BOLD)
    };
    let lead = vec![
        Span::plain(" "),
        Span::new("↶", head),
        Span::plain(" "),
        Span::new(label, head),
        Span::plain(" "),
    ];
    let lead_w = spans_width(&lead);
    let rule_for = |t: &str| -> isize {
        width as isize
            - 1
            - lead_w as isize
            - if t.is_empty() {
                0
            } else {
                string_width(t) as isize + 4
            }
    };
    let text = [note.as_str(), short]
        .into_iter()
        .find(|t| rule_for(t) >= RULE_MIN as isize)
        .unwrap_or("");
    let mut spans = lead;
    spans.push(Span::new("─".repeat(rule_for(text).max(3) as usize), ink));
    if !text.is_empty() {
        spans.push(Span::plain(" "));
        spans.push(Span::new(
            text.to_owned(),
            if blocked { theme.fg3 } else { theme.fg2 },
        ));
        spans.push(Span::plain(" "));
        spans.push(Span::new("──", ink));
    }
    StyledLine::new(spans)
}

/// `c` 向背景混合 `t`；没有颜色时原样。
fn mixed(theme: &Theme, c: Color, t: f64) -> Color {
    match (c, theme.bg) {
        (Color::TrueColor(r, g, b), Some(bg)) => Rgb::new(r, g, b).mix(bg, t).color(),
        _ => c,
    }
}

/// 留在后面的变暗：rich 模式把每种墨向背景混 70 % 并去掉粗体，板与代码面板的底比板淡，diff 行去掉它们的底；plain 模式是
/// SGR dim。
pub fn dimmer(theme: &Theme) -> impl Fn(StyledLine) -> StyledLine + Send + Sync + 'static {
    let colours = match (theme.fg, theme.bg, theme.panel) {
        (Some(fg), Some(bg), Some(panel)) => Some((
            fg.mix(bg, 0.7).color(),
            fg.mix(bg, 0.972).color(),
            panel,
            [theme.add_bg, theme.del_bg],
        )),
        _ => None,
    };
    move |line: StyledLine| {
        let restyle = |st: Style| match colours {
            Some((ink, ground, panel, diffs)) => {
                // 板的半块边是 panel 色的字：它跟着底色变淡。
                let f = if st.fg_color() == panel { ground } else { ink };
                let b = match st.bg_color() {
                    c if c == panel => ground,
                    c if diffs.contains(&Some(c)) => Color::Default,
                    c => c,
                };
                Style::new(f, b, st.attrs() & !attr::BOLD)
            }
            None => st.with_attr(attr::DIM),
        };
        StyledLine {
            spans: line
                .spans
                .into_iter()
                .map(|mut sp| {
                    sp.style = restyle(sp.style);
                    sp
                })
                .collect(),
            cont: line.cont,
        }
    }
}

fn label(theme: &Theme, text: &str) -> Vec<Span> {
    vec![
        Span::plain(INDENT),
        Span::new(pad(text, LABEL_WIDTH), theme.fg3),
        Span::plain("   "),
    ]
}

/// `+a −d`，只有在的部分。
pub fn change(theme: &Theme, added: u64, removed: u64) -> Vec<Span> {
    let parts: Vec<Span> = [
        (added > 0).then(|| Span::new(format!("+{added}"), Style::fg(theme.ok))),
        (removed > 0).then(|| Span::new(format!("−{removed}"), Style::fg(theme.err))),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut out = Vec::new();
    for (i, sp) in parts.into_iter().enumerate() {
        if i > 0 {
            out.push(Span::plain(" "));
        }
        out.push(sp);
    }
    out
}

/// 挑选时输入框上方的行：Also（`also` 是目标、压缩这些随之回去或留下的话），然后 Files；回不去的点没有。
pub fn rows(theme: &Theme, points: &[Point], k: usize, also: &[String]) -> Vec<Element> {
    rows_said(theme, points, k, also, &REWIND_WORDS)
}

/// 同 [`rows`]，字是 `w`。
pub fn rows_said(
    theme: &Theme,
    points: &[Point],
    k: usize,
    also: &[String],
    w: &RewindWords,
) -> Vec<Element> {
    let p = &points[k];
    if p.from == PointFrom::None {
        return Vec::new();
    }
    let sep = || Span::new("  ·  ", theme.line);
    let mut out = Vec::new();
    if !also.is_empty() {
        let mut spans = label(theme, w.also);
        for (i, t) in also.iter().enumerate() {
            if i > 0 {
                spans.push(sep());
            }
            spans.push(Span::new(t.clone(), theme.fg2));
        }
        out.push(single_line(spans, false));
    }
    if p.added > 0 || p.removed > 0 {
        let mut spans = label(theme, w.files);
        spans.push(Span::new(w.stay_files, theme.fg2));
        spans.push(sep());
        spans.push(Span::new((w.changed)(behind(points, k)), theme.fg3));
        spans.extend(change(theme, p.added, p.removed));
        out.push(single_line(spans, false));
    }
    out
}

/// fork 出的会话的痕迹行（V4 forkNotice）：它从哪个会话 fork 出来，知道的话。
pub fn trace_row(theme: &Theme, title: Option<&str>) -> Element {
    trace_row_said(theme, title, &REWIND_WORDS)
}

/// 同 [`trace_row`]，字是 `w`。
pub fn trace_row_said(theme: &Theme, title: Option<&str>, w: &RewindWords) -> Element {
    let mut spans = vec![
        Span::plain(" "),
        Span::new("↶", theme.fg3),
        Span::plain(" "),
        Span::new(w.rewound, theme.fg2.with_attr(attr::BOLD)),
    ];
    if let Some(t) = title {
        spans.push(Span::plain("  "));
        spans.push(Span::new((w.as_it_was)(t), theme.fg3));
    }
    single_line(spans, false)
}

/// 列表为 `query` 显示的点，最新的在前，各带它的序号。`text(k)` 是点 `k` 的消息。
pub fn matches(points: &[Point], text: &dyn Fn(usize) -> String, query: &str) -> Vec<usize> {
    let q = query.to_lowercase();
    (0..points.len())
        .rev()
        .filter(|k| q.is_empty() || text(*k).to_lowercase().contains(&q))
        .collect()
}

/// 点列表的行：选中的周围的窗口，然后是选中的点的细节。`now` 是墙钟。
pub fn list_rows(
    theme: &Theme,
    points: &[Point],
    text: &dyn Fn(usize) -> String,
    ui: &List<'_>,
    now: f64,
    offset: &dyn Fn(f64) -> i32,
) -> Vec<Element> {
    let none = format!("No message matches “{}”.", ui.query);
    list_rows_said(theme, points, text, ui, (now, offset), &none, &|k| {
        rewind_detail(theme, points, k)
    })
}

/// 选中的点 `k` 的细节（`/rewind`）：回不去时说为什么；否则留在 `/resume` 里的回合，与没有回退的文件改动。
pub fn rewind_detail(theme: &Theme, points: &[Point], k: usize) -> Vec<Span> {
    let p = &points[k];
    if p.from == PointFrom::None {
        return vec![Span::new(
            "can't go back here: the turn above did not finish",
            theme.fg3.with_attr(attr::ITALIC),
        )];
    }
    let n = behind(points, k);
    let mut d = vec![Span::new(
        format!(
            "{n} turn{} behind, in /resume",
            if n == 1 { " stays" } else { "s stay" }
        ),
        theme.fg3,
    )];
    if p.added > 0 || p.removed > 0 {
        d.push(Span::new("  ·  ", theme.line));
        d.push(Span::new("files stay ", theme.fg3));
        d.extend(change(theme, p.added, p.removed));
    }
    d
}

/// [`list_rows`] 的版面，没有相符的点时说 `none`，细节行是 `detail(k)`（tui-kit.md §4.1 第 4 条；旧界面的 `/fork` 用同一个
/// 版面，§4.3）。`clock` 是墙钟与时区偏移。
pub fn list_rows_said(
    theme: &Theme,
    points: &[Point],
    text: &dyn Fn(usize) -> String,
    ui: &List<'_>,
    clock: (f64, &dyn Fn(f64) -> i32),
    none: &str,
    detail: &dyn Fn(usize) -> Vec<Span>,
) -> Vec<Element> {
    let (now, offset) = clock;
    let ms = matches(points, text, ui.query);
    if ms.is_empty() {
        return vec![row(vec![Span::new(none.to_owned(), theme.fg3)])];
    }
    let sel = ui.choice.min(ms.len() - 1);
    let (from, n) = picker_window(sel, ms.len());
    let gap = || Span::plain(" ".repeat(AGE_WIDTH + 3));
    let more = |t: String| row(vec![gap(), Span::new(t, theme.fg3)]);
    let chosen = if theme.rich {
        Style::fg(theme.brand)
    } else {
        Style::new(theme.brand, Color::Default, attr::BOLD)
    };
    let mut out = Vec::new();
    if from > 0 {
        out.push(more(format!("↑ {from} more")));
    }
    for (i, k) in ms.iter().enumerate().skip(from).take(n) {
        let k = *k;
        let t = text(k);
        let first = t.lines().next().unwrap_or("");
        let blocked = points[k].from == PointFrom::None;
        let base = if blocked {
            if i == sel {
                theme.fg3.with_attr(attr::BOLD)
            } else {
                theme.fg3
            }
        } else if i == sel {
            chosen
        } else {
            theme.fg2
        };
        let hit = if i == sel {
            Style::new(theme.brand, Color::Default, attr::BOLD)
        } else {
            Style::BOLD
        };
        let mut spans = vec![
            Span::new(
                pad_left(&when::age(now, points[k].at as f64, offset), AGE_WIDTH),
                theme.fg3,
            ),
            Span::plain("   "),
        ];
        spans.extend(found(first, ui.query, base, hit));
        out.push(row(spans));
    }
    if from + n < ms.len() {
        out.push(more(format!("↓ {} more", ms.len() - from - n)));
    }
    let mut last = vec![gap()];
    last.extend(detail(ms[sel]));
    out.push(row(last));
    out
}
