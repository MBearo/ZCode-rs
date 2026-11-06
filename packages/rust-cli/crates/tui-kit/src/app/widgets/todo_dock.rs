//! 待办坞（docs/puffin-tui.md §4.2「待办」；docs/specs/rust-cli/tui-kit.md §4.4）：输入框上方 agent 的清单：它在哪一步、还剩
//! 什么，不论 transcript 滚到哪里。至多 5 行（在做的、它之前最近完成的一步、之后的），其余的数在最后一行；第一行右沿说
//! `2 of 6 done`；矮的终端、停下的清单只有一行；全部完成时回合还在跑就说一声。纯的；入参是中性的 [`Todos`]。
//! 移植自 Puffin 的 shell/todo_dock.rs（TodoDock.scala 第 1–118 行）。
use std::collections::BTreeSet;

use crate::app::element::{Element, by_width, single_line, stack};
use crate::render::width::string_width;
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::fit::{self, clip, java_space};
use super::key_hints;
use super::todo_step::{Status, status, step};
use super::tools::text::{lines_iter, plural};

/// 清单：每一步的状态（`pending`、`inProgress` 或 `completed`；`Todos` 调用的 `in_progress` 读作一样）与文字，以及它是不是
/// 最新的回合写的。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Todos {
    pub steps: Vec<(String, String)>,
    pub fresh: bool,
}

/// 至多这么多行；超过时最后一行数其余的。
pub const MAX_ROWS: usize = 5;
/// 更矮的终端只有一行。
pub const TALL_ROWS: usize = 30;
/// 一行的文字与右边的计数之间留的格数。
const GAP: i64 = 3;
const MIN_TEXT: i64 = 10;

/// 坞的字（只有字，排法相同）：`count(完成, 总数)` 是第一行右沿的计数，`all_done(总数)` 是全部完成时那一行，`more(n)` 是
/// 没显示的步骤（多行时最后一行写它，一行时在 `+` 后面）。
#[derive(Clone, Copy)]
pub struct DockWords {
    pub count: fn(usize, usize) -> String,
    pub all_done: fn(usize) -> String,
    pub more: fn(usize) -> String,
}

/// Puffin 的字。
pub const DOCK_WORDS: DockWords = DockWords {
    count: |done, n| format!("{done} of {n} done"),
    all_done: |n| format!("All {} done", plural(n as i64, "step")),
    more: |n| format!("{n} more"),
};

/// 坞。`running` 是回合在跑或在等你；`one_row` 是终端矮或下面停着工作流。
pub fn dock(theme: &Theme, todos: Option<&Todos>, running: bool, one_row: bool) -> Element {
    dock_said(theme, todos, running, one_row, &DOCK_WORDS)
}

/// [`dock`]，字是 `words` 的（别的界面语言）。
pub fn dock_said(
    theme: &Theme,
    todos: Option<&Todos>,
    running: bool,
    one_row: bool,
    words: &DockWords,
) -> Element {
    let Some(t) = todos.filter(|t| !t.steps.is_empty()) else {
        return Element::Blank;
    };
    let steps: Vec<(Status, String)> = t
        .steps
        .iter()
        .map(|(s, text)| (status(s), one_line(text)))
        .collect();
    let n = steps.len();
    let done = steps.iter().filter(|s| s.0 == Status::Done).count();
    let count = Span::new((words.count)(done, n), theme.fg3);
    if done == n {
        // 全部完成：回合还在运行时说一声，回合结束就离开。
        return if running && t.fresh {
            line(
                vec![
                    Span::new("✓", Style::fg(theme.ok)),
                    Span::new(" ", Style::DEFAULT),
                    Span::new((words.all_done)(n), theme.fg3),
                ],
                None,
                Vec::new(),
            )
        } else {
            Element::Blank
        };
    }
    if !running && !t.fresh {
        // 你发了下一条消息，那个回合没碰它：停下的清单离开。
        return Element::Blank;
    }
    let quiet = !running || !t.fresh;
    if one_row || quiet {
        let now: Vec<usize> = (0..n).filter(|i| steps[*i].0 == Status::Now).collect();
        let k = now
            .first()
            .copied()
            .or_else(|| steps.iter().position(|s| s.0 == Status::Todo))
            .unwrap_or(0);
        let more = if now.len() > 1 {
            vec![Span::new(
                format!("  +{}", (words.more)(now.len() - 1)),
                theme.fg3,
            )]
        } else {
            Vec::new()
        };
        line(
            step(theme, Status::Now, &steps[k].1, quiet),
            Some(count),
            more,
        )
    } else {
        let kinds: Vec<Status> = steps.iter().map(|s| s.0).collect();
        let (shown, more) = pick(&kinds);
        let mut rows: Vec<Element> = shown
            .iter()
            .enumerate()
            .map(|(r, &i)| {
                line(
                    step(theme, steps[i].0, &steps[i].1, false),
                    (r == 0).then(|| count.clone()),
                    Vec::new(),
                )
            })
            .collect();
        if more > 0 {
            rows.push(single_line(
                vec![
                    Span::new(" ", Style::DEFAULT),
                    Span::new("·", theme.fg3),
                    Span::new(" ", Style::DEFAULT),
                    Span::new((words.more)(more), theme.fg3),
                ],
                false,
            ));
        }
        stack(rows)
    }
}

/// 行显示哪些步骤（按清单的次序）与有多少没完成的步骤没显示。至多 [`MAX_ROWS`] 行，其余的计数占其中一行。先是在做的步骤
/// （没有时第一个没开始的代替），然后是它们之前最近完成的一步，然后是它们之后的步骤。
pub fn pick(steps: &[Status]) -> (Vec<usize>, usize) {
    let n = steps.len();
    if n <= MAX_ROWS {
        return ((0..n).collect(), 0);
    }
    let now: Vec<usize> = (0..n).filter(|i| steps[*i] == Status::Now).collect();
    let anchor: Vec<usize> = if now.is_empty() {
        steps
            .iter()
            .position(|s| *s == Status::Todo)
            .into_iter()
            .collect()
    } else {
        now
    };
    let first = anchor.first().copied().unwrap_or(n);
    let last_done = (0..first).rev().find(|i| steps[*i] == Status::Done);
    let after = anchor.last().map_or(0, |i| i + 1)..n;
    // 其余的步骤补在后面：没完成的按次序，完成的从新到旧，所以行数总是用满。
    let rest = (0..n)
        .filter(|i| steps[*i] != Status::Done)
        .chain((0..n).rev().filter(|i| steps[*i] == Status::Done));
    let mut order: Vec<usize> = Vec::new();
    for i in anchor
        .iter()
        .copied()
        .chain(last_done)
        .chain(after)
        .chain(rest)
    {
        if !order.contains(&i) {
            order.push(i);
        }
    }
    let open = |chosen: &BTreeSet<usize>| {
        (0..n)
            .filter(|i| !chosen.contains(i) && steps[*i] != Status::Done)
            .count()
    };
    let five: BTreeSet<usize> = order.iter().take(MAX_ROWS).copied().collect();
    if open(&five) == 0 {
        (five.into_iter().collect(), 0)
    } else {
        let four: BTreeSet<usize> = order.iter().take(MAX_ROWS - 1).copied().collect();
        let left = open(&four);
        (four.into_iter().collect(), left)
    }
}

/// ` mark text`，`right` 结束在槽的右沿（§4.2「按键提示（槽）」）。先截文字，至少留 [`MIN_TEXT`] 格；然后去掉 `right`。
fn line(step_spans: Vec<Span>, right: Option<Span>, after: Vec<Span>) -> Element {
    by_width(move |w| {
        let Some((text, init)) = step_spans.split_last() else {
            return Element::Blank;
        };
        let mut lead = vec![Span::new(" ", Style::DEFAULT)];
        lead.extend(init.iter().cloned());
        let e = key_hints::edge(w) as i64;
        let fit = |room: i64| {
            let t = if string_width(&text.text) as i64 <= room {
                text.text.to_string()
            } else {
                let c = clip(&text.text, room.max(0) as usize);
                let body = c.strip_suffix('…').unwrap_or(&c);
                format!("{}…", body.trim_end_matches(java_space))
            };
            let mut v = lead.clone();
            v.push(Span::new(t, text.style));
            v.extend(after.iter().cloned());
            v
        };
        let lead_w = fit::width(&lead) as i64;
        let tail = fit::width(&after) as i64;
        let room_with = |r: &Span| e - lead_w - tail - string_width(&r.text) as i64 - GAP;
        match right.as_ref().filter(|r| room_with(r) >= MIN_TEXT) {
            Some(r) => {
                let mut left = fit(room_with(r));
                let pad = (e - fit::width(&left) as i64 - string_width(&r.text) as i64).max(0);
                left.push(Span::new(" ".repeat(pad as usize), Style::DEFAULT));
                left.push(r.clone());
                single_line(left, false)
            }
            None => single_line(fit((e - lead_w - tail).max(1)), false),
        }
    })
}

/// 文字的第一行，不带 tab 与回车：格子里放不下它们。
fn one_line(text: &str) -> String {
    lines_iter(text)
        .first()
        .copied()
        .unwrap_or("")
        .replace('\r', "")
        .replace('\t', "    ")
}
