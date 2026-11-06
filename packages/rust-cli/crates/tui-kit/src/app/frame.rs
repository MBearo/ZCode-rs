//! 一屏的骨架（docs/puffin-tui.md §4.4；docs/specs/rust-cli/tui-kit.md §4.0）：置顶行、分隔行、底部栈与正文怎样分一屏、
//! 开着的一层怎样盖住正文最新的几行、输入框上面那一行、输入框上方的提示。两套界面都调这里（tui-kit.md §4.4）。
//!
//! 从 Puffin 的壳（shell/view/frame.rs 的 `transcript_frame`、`pinned_row`、`separator_row`；
//! `shell/composer.rs` 的 `composer_gap`；`shell/view.rs` 的 `problems`）搬来，改成只收朴素的数据（行、行数、真假）；
//! 行为不变。
use crate::app::element::{Element, br, single_line, stack, text};
use crate::app::layout::Layout;
use crate::markdown::render::INDENT;
use crate::render::ansi::RESET;
use crate::render::width::string_width;
use crate::render::{Span, Style, StyledLine, attr};
use crate::theme::theme::Theme;

/// 输入框与置顶行的提示符。
pub const PROMPT_ARROW: &str = "›";

/// 正文不少于这么多行时才有置顶行。
pub const PIN_MIN_BODY: usize = 3;

fn first_line(el: &Element, width: usize) -> StyledLine {
    Layout::lay(el, width)
        .into_iter()
        .next()
        .unwrap_or_default()
}

/// 置顶行写哪条消息：`at(top)` 是顶行在某条消息的开头之后时那条消息。要不要置顶按整个正文的顶行 `top0` 决定（[`extras`]），
/// 置顶行与分隔行拿走的行让跟随的视图往下移到 `top`；`top` 正好落在下一条消息上面的空行（它的开头）时，按 `top` 找不到消息。
/// 这时写 `top0` 那一条：它是让置顶行出现的消息。以前两个 shell 都只按 `top` 找，那一帧的置顶行是空的 ` ›`。
pub fn pinned_at<T>(at: impl Fn(usize) -> Option<T>, top0: usize, top: usize) -> Option<T> {
    at(top).or_else(|| at(top0))
}

/// `› message`，在板的颜色上，用 `…` 截断（Puffin `pinned_row`）。
pub fn pinned_row(theme: &Theme, words: &str, width: usize) -> StyledLine {
    let bg = theme.panel;
    let st = |base: Style| bg.map_or(base, |b| base.with_bg(b));
    let spans = vec![
        Span::new(" ", st(Style::DEFAULT)),
        Span::new(
            PROMPT_ARROW,
            st(Style::fg(theme.brand).with_attr(attr::BOLD)),
        ),
        Span::new(" ", st(Style::DEFAULT)),
        Span::new(
            words.to_owned(),
            st(if theme.rich {
                Style::DEFAULT
            } else {
                theme.fg2
            }),
        ),
    ];
    let line = first_line(&single_line(spans, false), width);
    let pad = width.saturating_sub(line.width());
    if pad > 0 && bg.is_some() {
        let mut spans = line.spans;
        spans.push(Span::new(" ".repeat(pad), st(Style::DEFAULT)));
        StyledLine::new(spans)
    } else {
        line
    }
}

/// `↓ N more   ^N back to the end`，居中，在视图不在末尾时；放不下或 `short`（Puffin 挑选回退的点）时只有 `↓ N more`
/// （docs/puffin-tui.md §4.4「分隔行」，Puffin `separator_row`；键是 ^N，tui-kit.md §4.0）。
pub fn separator_row(theme: &Theme, below: usize, width: usize, short: bool) -> StyledLine {
    let more = Span::new(format!("↓ {below} more"), theme.fg3);
    let full = vec![
        more.clone(),
        Span::new("   ", Style::DEFAULT),
        Span::new("^N", theme.fg2),
        Span::new(" back to the end", theme.fg3),
    ];
    let w = |spans: &[Span]| spans.iter().map(|sp| string_width(&sp.text)).sum::<usize>();
    let spans = if w(&full) <= width && !short {
        full
    } else {
        vec![more]
    };
    let pad = width.saturating_sub(w(&spans)) / 2;
    let mut all = vec![Span::new(" ".repeat(pad), Style::DEFAULT)];
    all.extend(spans);
    first_line(&single_line(all, false), width)
}

/// 底部栈至多占的行数：`rows − 1`（docs/puffin-tui.md §4.4「底部栈」）。
pub fn max_bottom(rows: usize) -> usize {
    rows.saturating_sub(1).max(1)
}

/// 一屏怎样分：没有层的一摞 `stack` 行与状态栏 `status` 行，在 `rows` 行的屏幕上。返回底部栈的行数与正文的高度（置顶行
/// 与分隔行还在正文里）。
pub fn split(rows: usize, stack: usize, status: usize) -> (usize, usize) {
    let rows = rows.max(1);
    let bottom = (stack + status).min(max_bottom(rows));
    (bottom, rows - bottom)
}

/// 置顶行与分隔行各占几行（0 或 1）：正文（按整个正文的高度 `body`）不少于 [`PIN_MIN_BODY`] 行、有要置顶的消息时有置顶行；
/// 锚定（`anchored`）、下面还有（`more_below`）、正文还容得下、没有请求开着（`asking`）时有分隔行。
pub fn extras(
    body: usize,
    pinned: bool,
    anchored: bool,
    more_below: bool,
    asking: bool,
) -> (usize, usize) {
    let pin = usize::from(body >= PIN_MIN_BODY && pinned);
    let sep = usize::from(anchored && more_below && body >= pin + 3 && !asking);
    (pin, sep)
}

/// 拼好的一屏与层比没有层的一摞多出的行数（盖住的正文行数，可为负）。
pub struct Spliced {
    pub lines: Vec<StyledLine>,
    pub over: i64,
}

/// 一屏：`upper`（置顶行、正文、分隔行，正文按没有层的一摞量高）之下接带层的一摞 `shelf` 与状态栏 `status`。层比没有层
/// 的一摞（`stack0` 行）高出的行盖在正文最新的几行上，不推它们上去（docs/puffin-tui.md §4.2；Puffin `transcript_frame`）。
/// `bottom` 是 [`split`] 给的底部栈的行数。
pub fn splice(
    mut upper: Vec<StyledLine>,
    stack0: usize,
    shelf: Vec<StyledLine>,
    status: Vec<StyledLine>,
    bottom: usize,
) -> Spliced {
    let over: i64 = (shelf.len() as i64 - stack0 as i64).min(upper.len() as i64);
    let mut tail = shelf;
    tail.extend(status);
    let keep = (bottom as i64 + over).max(0) as usize;
    let lower = tail.split_off(tail.len().saturating_sub(keep));
    let drop = over.max(0) as usize;
    upper.truncate(upper.len().saturating_sub(drop));
    upper.extend(lower);
    Spliced { lines: upper, over }
}

/// 输入框上面空一行，或空在它上面的提示之上（提示自带）。plain 模式总有；rich 模式只在 transcript 有正文（`has_body`）、
/// 没有提示（`quiet`）时有（Puffin `composer_gap`；会话的名字不在这一行，在壳的标题行）。
pub fn composer_gap(rich: bool, has_body: bool, quiet: bool) -> Element {
    if !rich || (has_body && quiet) {
        br()
    } else {
        Element::Blank
    }
}

/// 输入框上方带记号的一行：第 1 列 err 的 `✗`，第 3 列起 `style` 的一句（Puffin `problems` 的 `mark_row`）。
pub fn mark_row(theme: &Theme, words: &str, style: Style) -> Element {
    text(format!(
        " {}✗{RESET} {}{words}{RESET}",
        Style::fg(theme.err).set_sequence(),
        style.set_sequence()
    ))
}

/// 一时的失败：` ✗ ` 与 fg2 的一句。
pub fn notice_row(theme: &Theme, words: &str) -> Element {
    mark_row(theme, words, theme.fg2)
}

/// 接上了什么：第 1 列 ok 的 `✓`，第 3 列起 fg2 的一句（Puffin「登录成功」）。
pub fn ok_row(theme: &Theme, words: &str) -> Element {
    text(format!(
        " {}✓{RESET} {}{words}{RESET}",
        Style::fg(theme.ok).set_sequence(),
        theme.fg2.set_sequence()
    ))
}

/// 安静的一行：第 3 列起 fg3 的一句，一行（Puffin 的 `quiet`）。
pub fn quiet_row(theme: &Theme, words: &str) -> Element {
    single_line(
        vec![
            Span::new(INDENT, Style::DEFAULT),
            Span::new(words.to_owned(), theme.fg3),
        ],
        false,
    )
}

/// 输入框上方的提示（Puffin `problems`）：全是空的时什么都没有；否则有正文（`has_body`）时上面空一行。
pub fn problems(has_body: bool, rows: impl IntoIterator<Item = Element>) -> Element {
    let rows: Vec<Element> = rows.into_iter().collect();
    if rows.iter().all(Element::is_blank) {
        return Element::Blank;
    }
    let gap = if has_body { br() } else { Element::Blank };
    stack(std::iter::once(gap).chain(rows))
}

// ---- 页：正文换成一页，底部栈留着（docs/puffin-tui.md §4.4；Puffin `usage_parts`、`usage_frame`） ----

/// 页下面的底部栈：输入框的一摞与状态栏，至多 [`max_bottom`] 行，多出时留下面的。
pub fn page_bottom(mut bottom: Vec<StyledLine>, rows: usize) -> Vec<StyledLine> {
    let keep = max_bottom(rows);
    bottom.split_off(bottom.len().saturating_sub(keep))
}

/// 一页代替正文的一屏：页的行从 `top`（夹到最后一屏）起填满底部栈上面的 `rows − bottom.len()` 行，看不见的行在第一或
/// 最后一行写 fg3 的 `↑ n more` / `↓ n more`，不够的补空行，下面接 `bottom`（[`page_bottom`] 截过的）。返回一屏的行与
/// 夹住之后的顶行。
pub fn page(
    theme: &Theme,
    page: &[StyledLine],
    top: usize,
    bottom: Vec<StyledLine>,
    rows: usize,
) -> (Vec<StyledLine>, usize) {
    let rows = rows.max(1);
    let body_h = rows.saturating_sub(bottom.len());
    let top = top.min(page.len().saturating_sub(body_h));
    let more = |t: String| {
        StyledLine::new(vec![
            Span::new(INDENT, Style::DEFAULT),
            Span::new(t, theme.fg3),
        ])
    };
    let mut lines: Vec<StyledLine> = page.iter().skip(top).take(body_h).cloned().collect();
    if top > 0 && !lines.is_empty() {
        lines[0] = more(format!("↑ {} more", top + 1));
    }
    let hidden = page.len() as i64 - (top + body_h) as i64;
    if hidden > 0 && !lines.is_empty() {
        let n = lines.len();
        lines[n - 1] = more(format!("↓ {} more", hidden + 1));
    }
    let fill = body_h.saturating_sub(lines.len());
    lines.extend(std::iter::repeat_n(StyledLine::default(), fill));
    lines.extend(bottom);
    let lines = lines.split_off(lines.len().saturating_sub(rows));
    (lines, top)
}

// ---- 尾巴：重试行（docs/puffin-tui.md §4.2「重试」；Puffin `failures.rs` 的 `retry_words`、`retry_line`、`countdown`） ----

/// 一段重试持续这么多 tick（1 s）之后重试行才出现（Puffin `RETRY_GRACE_TICKS`）。
pub const RETRY_GRACE_TICKS: i64 = 30;

/// 重试的原因，按它的 reasonCode：`provider` 是 provider 的短名（句首大写的写法由调用方给），`host` 是它的主机。
pub fn retry_words(reason: &str, provider: &str, host: &str) -> String {
    match reason {
        "fault.provider.rateLimited" => format!("{provider} is limiting requests"),
        // 与 Puffin 不同（tui-kit.md §4.0「尾巴」）：Puffin 的过载落在最后一项（`had a problem`），说不清是什么问题。
        "fault.provider.overloaded" => format!("{provider} is overloaded"),
        "fault.network.unreachable" | "fault.network.timeout" => format!("Could not reach {host}"),
        "fault.network.sseDisconnected" => "The connection dropped".to_owned(),
        "fault.network.sseStalled" => "The answer stopped coming".to_owned(),
        _ => format!("{provider} had a problem"),
    }
}

/// `text` 截到 `cols` 格，截断时最后一格是 `…`（Puffin `consts::clip`）。
fn clip(text: &str, cols: usize) -> String {
    if string_width(text) <= cols {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in text.chars() {
        let cw = string_width(ch.encode_utf8(&mut [0; 4]));
        if w + cw < cols {
            out.push(ch);
            w += cw;
        }
    }
    out.push('…');
    out
}

fn spans_width(spans: &[Span]) -> usize {
    spans.iter().map(|sp| string_width(&sp.text)).sum()
}

/// `○ 原因  ·  trying again in 14s  ·  tried 3 times`（Puffin `retry_line`）：倒计时与次数的说法由调用方给（旧界面有中文），
/// `ring` 是光标的呼吸。放不下时先丢次数，再用 `…` 截原因；倒计时保留。
pub fn retry_line_with(
    theme: &Theme,
    reason: String,
    wait: String,
    count: Option<String>,
    ring: Style,
) -> Element {
    let sep = Span::new("  ·  ", theme.line);
    let wait = vec![sep.clone(), Span::new(wait, theme.fg3)];
    let count = count.map_or_else(Vec::new, |c| vec![sep, Span::new(c, theme.fg3)]);
    let fg2 = theme.fg2;
    crate::app::element::by_width(move |w| {
        let room = w as i64 - 4;
        let both: Vec<Span> = wait.iter().chain(count.iter()).cloned().collect();
        let tail = if (string_width(&reason) + spans_width(&both)) as i64 <= room {
            both
        } else {
            wait.clone()
        };
        let shown = if (string_width(&reason) + spans_width(&tail)) as i64 <= room {
            reason.clone()
        } else {
            clip(&reason, (room - spans_width(&tail) as i64).max(1) as usize)
        };
        let mut spans = vec![
            Span::new(" ", Style::DEFAULT),
            Span::new("○", ring),
            Span::new(" ", Style::DEFAULT),
            Span::new(shown, fg2),
        ];
        spans.extend(tail);
        single_line(spans, false)
    })
}

/// 到 `at`（毫秒）为止的整秒，向上取；过了之后是 0（Puffin `failures::countdown`）。
pub fn countdown(at: f64, now_ms: f64) -> i64 {
    (((at - now_ms) / 1000.0).ceil() as i64).max(0)
}
