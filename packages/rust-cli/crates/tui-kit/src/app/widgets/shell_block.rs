//! `!` 命令的块（docs/puffin-tui.md §4.2「Shell 命令（!）」；docs/specs/rust-cli/tui-kit.md §4.4）：命令在你的板上（第 1 列
//! 琥珀色的 `!`），结果在板那一行的末尾（`exit 1 · 3s`、`no output`、`stopped`…），输出在竖线上——收起时最新的 10 行，其余
//! 在它们上面数出来；在跑时 fg3、新行升进它，结束后落定到 fg2。入参是中性的 [`ShellRun`] 与 [`ToolClock`]。
//! 移植自 Puffin 的 shell/shell_view.rs 的块的纯函数部分（ShellView.scala 第 1–140 行）。
use crate::app::element::{Element, by_width, single_line, slab, stack};
use crate::markdown::render::SETTLED;
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::fade;
use super::fit::{self, clip};
use super::tool_row::{self, ToolClock};
use super::tools::text::lines_iter;

/// 收起时显示的输出行数；^O 显示全部。
pub const TAIL: usize = 10;

/// 命令的状态。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShellStatus {
    Running,
    Done,
    Stopped,
    Failed,
}

/// 一条 `!` 命令与它运行的事实：`exit_code` 不知道时为 -1；`lines` 是留下的输出行，`dropped` 是丢掉的行数；时刻是 epoch
/// 毫秒，没有时为 0；`error` 是跑不起来时为什么。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ShellRun<'a> {
    pub command: &'a str,
    pub status: ShellStatus,
    pub ended: bool,
    pub exit_code: i32,
    pub lines: &'a [String],
    pub dropped: u64,
    pub started_at: u64,
    pub ended_at: u64,
    pub error: &'a str,
}

impl ShellRun<'_> {
    /// 输出的总行数，含丢掉的。
    pub fn total(&self) -> usize {
        self.lines.len() + usize::try_from(self.dropped).unwrap_or(usize::MAX)
    }

    /// 它的输出比收起的多：^O 显示全部。
    pub fn longer(&self) -> bool {
        self.lines.len() > TAIL || self.dropped > 0
    }

    /// 它下面什么都不挂：底也是板（板下面紧接的板不要空行，§4.2 板）。
    pub fn bare(&self) -> bool {
        self.lines.is_empty() && self.dropped == 0 && self.error.is_empty()
    }
}

/// `!` 标记：琥珀色粗体，或从 brand 淡入的第 `age` 个 tick。
pub fn mark(theme: &Theme, age: i64) -> Style {
    fade::ink(
        theme,
        theme.brand,
        Style::fg(theme.code).with_attr(attr::BOLD),
        age,
        fade::TICKS,
    )
}

/// 淡入结束的 `!` 标记。
pub fn mark_settled(theme: &Theme) -> Style {
    mark(theme, SETTLED)
}

/// 命令的块。`clock` 是它的动效，`now` 是 tick，`now_ms` 是墙钟（在跑的命令的时间）；`details` 是 ^O。
pub fn block(
    theme: &Theme,
    r: &ShellRun<'_>,
    clock: &ToolClock,
    now: i64,
    now_ms: f64,
    details: bool,
) -> Element {
    let head = command_rows(theme, r.command, outcome(theme, r, clock, now, now_ms));
    let under = output_rows(theme, r, clock, now, details);
    if under.is_empty() {
        head
    } else {
        stack(std::iter::once(head).chain(under))
    }
}

/// 刚跑起来、还没有事实的命令：在跑，没有输出。
pub fn held(theme: &Theme, command: &str) -> Element {
    command_rows(theme, command, Vec::new())
}

/// 你的板上的命令：第 1 列是 `!`，命令从第 3 列起，结果靠右在它最后一行，命令在它前面用 `…` 截断。
fn command_rows(theme: &Theme, command: &str, end: Vec<Span>) -> Element {
    let ink = if theme.rich {
        Style::DEFAULT
    } else {
        theme.fg2
    };
    let lines: Vec<&str> = command.split('\n').collect();
    let last = lines.len() - 1;
    let rows: Vec<Element> = lines
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let lead = if i == 0 {
                vec![
                    Span::new(" ", Style::DEFAULT),
                    Span::new("!", mark_settled(theme)),
                    Span::new(" ", Style::DEFAULT),
                ]
            } else {
                vec![Span::new("   ", Style::DEFAULT)]
            };
            let right = if i == last { end.clone() } else { Vec::new() };
            let text = line.replace('\t', "    ").replace('\r', "");
            by_width(move |w| {
                let end_w: usize = right.iter().map(|sp| string_width(&sp.text)).sum();
                let room = if end_w == 0 {
                    w as i64 - 3 - 1
                } else {
                    w as i64 - 3 - end_w as i64 - 3
                };
                let shown = if room >= 1 {
                    clip(&text, room as usize)
                } else {
                    String::new()
                };
                let mut spans = lead.clone();
                if end_w == 0 {
                    spans.push(Span::new(shown, ink));
                } else {
                    let gap =
                        (w as i64 - 3 - string_width(&shown) as i64 - end_w as i64 - 1).max(2);
                    spans.push(Span::new(shown, ink));
                    spans.push(Span::new(" ".repeat(gap as usize), Style::DEFAULT));
                    spans.extend(right.iter().cloned());
                }
                single_line(spans, false)
            })
        })
        .collect();
    let body = stack(rows);
    match theme.panel {
        Some(p) => slab(body, p),
        None => body,
    }
}

/// 发生了什么，在板那一行的末尾：很快的成功什么都没有；1 s 起是时间；`no output`；`exit N`；`stopped`；`failed`。在跑时
/// 3 s 起是时间。命令结束时淡入。
fn outcome(theme: &Theme, r: &ShellRun<'_>, clock: &ToolClock, now: i64, now_ms: f64) -> Vec<Span> {
    if !r.ended {
        let ms = now_ms - r.started_at as f64;
        return if r.started_at > 0 && ms >= 3000.0 {
            vec![Span::new(running(ms), theme.fg3)]
        } else {
            Vec::new()
        };
    }
    let ms = r.ended_at as f64 - r.started_at as f64;
    let t = fit::time(ms);
    let time = vec![(format!(" · {t}"), theme.fg3)];
    let err = Style::fg(theme.err);
    let with_time = |head: (String, Style)| std::iter::once(head).chain(time.clone()).collect();
    let all = r.total();
    let parts: Vec<(String, Style)> = match r.status {
        ShellStatus::Failed => vec![("failed".to_owned(), err)],
        ShellStatus::Stopped => with_time(("stopped".to_owned(), theme.fg3)),
        _ if r.exit_code > 0 => with_time((format!("exit {}", r.exit_code), err)),
        _ if r.exit_code < 0 => with_time(("killed".to_owned(), err)),
        _ if all == 0 => {
            if ms >= 1000.0 {
                with_time(("no output".to_owned(), theme.fg3))
            } else {
                vec![("no output".to_owned(), theme.fg3)]
            }
        }
        _ if ms >= 1000.0 => vec![(t, theme.fg3)],
        _ => Vec::new(),
    };
    fade::parts(theme, parts, tool_row::age(clock.ended, now), None)
}

/// 在跑的命令的时间，整秒：`6s`、`1m 05s`。
fn running(ms: f64) -> String {
    let secs = (ms / 1000.0) as i64;
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}

/// 竖线上的输出：最新的 [`TAIL`] 行（^O 时全部），其余的在它们上面数出来；在跑时 fg3，新行升进它；结束后 fg2。跑不起来的
/// 命令说为什么。
fn output_rows(
    theme: &Theme,
    r: &ShellRun<'_>,
    clock: &ToolClock,
    now: i64,
    details: bool,
) -> Vec<Element> {
    let lines = r.lines;
    let shown: &[String] = if details {
        lines
    } else {
        &lines[lines.len().saturating_sub(TAIL)..]
    };
    let hidden = usize::try_from(r.dropped).unwrap_or(usize::MAX) + lines.len() - shown.len();
    let all = r.total();
    let railed = |span: Span| {
        let mut v = tool_row::rail(theme);
        v.push(span);
        single_line(v, false)
    };
    let mut out = Vec::new();
    if hidden > 0 {
        out.push(railed(Span::new(
            format!(
                "… {hidden} earlier line{}",
                if hidden == 1 { "" } else { "s" }
            ),
            theme.fg3,
        )));
    }
    for (k, l) in shown.iter().enumerate() {
        let text = l.replace('\t', "    ");
        let style = if r.ended {
            fade::ink(
                theme,
                theme.fg3.fg_color(),
                theme.fg2,
                tool_row::age(clock.ended, now),
                fade::TICKS,
            )
        } else {
            let landed = clock.landed_at(all - shown.len() + k);
            fade::fade_in(
                theme,
                vec![Span::new(text.clone(), theme.fg3)],
                tool_row::age(landed, now),
                fade::LAND_TICKS,
                None,
            )[0]
            .style
        };
        out.push(railed(Span::new(text, style)));
    }
    if r.status == ShellStatus::Failed && !r.error.is_empty() {
        let first = lines_iter(r.error)
            .first()
            .copied()
            .unwrap_or("")
            .to_owned();
        out.push(railed(Span::new(first, theme.fg2)));
    }
    out
}
