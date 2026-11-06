//! 并行调用的名册（docs/puffin-tui.md §4.2「并行调用」；docs/specs/rust-cli/tui-kit.md §4.4）。
//! 一批同时发出的调用画成它那一段的行下面的名册（§4.2「工作的收拢」的「一批调用」）：每个调用一行，按模型的次序，各说它此刻
//! 在做什么；末行 `└ ⋮⋮⋮ 2 running · 1 done` 数整批。纯的，同 [`super::gather`]；入参是中性的 [`Call`]。
//! 移植自 Puffin 的 shell/roster.rs（Roster.scala 第 1–130 行）。
use std::collections::BTreeSet;

use crate::app::element::{Element, by_width, single_line};
use crate::render::width::string_width;
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::fade;
use super::fit::{self, measured};
use super::pen::{self, Pen};
use super::subagent_cols;
use super::tool_row::{self, ToolClock, ToolCtx};
use super::tools::{self, Call, Status, Under, text::jtrim};

/// 名册最多的行数。
pub const MAX: usize = 4;

/// 最新的一批调用（同一次模型回答，至少两个、还有没结束的）；不是一批时为空。
pub fn of<'a>(calls: &[Call<'a>]) -> Vec<Call<'a>> {
    let r = calls.last().map_or("", |c| c.response);
    if r.is_empty() {
        return Vec::new();
    }
    let batch: Vec<Call<'a>> = calls.iter().filter(|c| c.response == r).copied().collect();
    if batch.len() >= 2 && batch.iter().any(|c| !c.ended) {
        batch
    } else {
        Vec::new()
    }
}

/// 它在跑：没有结束，也不在排队或等回答。
pub fn running(c: &Call<'_>) -> bool {
    !c.ended && !matches!(c.status, Status::Queued | Status::Asking)
}

fn sp(text: impl Into<String>, style: Style) -> Span {
    Span::new(text.into(), style)
}

/// 行：每个调用 `├ Name  target   state`，然后末行 `└ ⋮⋮⋮ 2 running · 1 done`。`pen` 在手停住时点亮末行的点阵。
pub fn rows(
    ctx: &ToolCtx<'_>,
    batch: &[Call<'_>],
    clocks: &dyn Fn(&str) -> ToolClock,
    pen: Option<&Pen>,
) -> Vec<Element> {
    let theme = ctx.theme;
    // 多于上限时行数不变：MAX 行给还没结束的调用（按模型的次序，不够时由结束的补上），末行数整批。
    let shown: Vec<&Call<'_>> = if batch.len() <= MAX {
        batch.iter().collect()
    } else {
        let picked: BTreeSet<&str> = batch
            .iter()
            .filter(|m| !m.ended)
            .chain(batch.iter().filter(|m| m.ended))
            .take(MAX)
            .map(|m| m.id)
            .collect();
        batch.iter().filter(|m| picked.contains(m.id)).collect()
    };
    let name_w = shown
        .iter()
        .map(|m| string_width(&tools::name(m)))
        .max()
        .unwrap_or(0);
    let targets: Vec<String> = shown
        .iter()
        .map(|m| tools::target(m, ctx.where_, ctx.tasks, ctx.todos))
        .collect();
    let longest = targets.iter().map(|t| string_width(t)).max().unwrap_or(0);
    let any_subagent = shown.iter().any(|m| tools::subagent(m));
    let mut out: Vec<Element> = shown
        .iter()
        .zip(targets)
        .map(|(m, target)| {
            let quiet = m.ended || tools::status(m) == "queued";
            let clock = clocks(m.id);
            let name = tools::name(m);
            let writing = tools::writing(m);
            let is_agent = tools::subagent(m);
            // 每种宽度下的状态先算好：画法闭包不能借 ctx。
            let st = State::of(ctx, m, &clock);
            let cells = is_agent.then(|| (ctx.cells)(m, clock.ended));
            let row = by_width(move |w| {
                let target_w = longest
                    .min(((w as f64 - name_w as f64 - 10.0) * 0.45).trunc().max(10.0) as usize);
                let shown_target = fit::clip(&target, target_w);
                let mut lead = vec![
                    sp("   ", Style::DEFAULT),
                    sp("├", theme.line),
                    sp(" ", Style::DEFAULT),
                ];
                // 输入还在写出的行只有名称，还没有对象（§4.2「笔」的「调用」）。
                let label: Vec<Span> = if writing {
                    vec![sp(name.clone(), theme.fg2)]
                } else {
                    let gap = " ".repeat(name_w.saturating_sub(string_width(&name)) + 2);
                    let mut v = vec![sp(name.clone(), if quiet { theme.fg3 } else { theme.fg2 })];
                    if !shown_target.is_empty() {
                        v.push(sp(gap, Style::DEFAULT));
                        v.push(sp(shown_target, theme.fg3));
                    }
                    v
                };
                let pad = (name_w + 2 + target_w).saturating_sub(fit::width(&label));
                lead.extend(label);
                lead.push(sp(format!("{}   ", " ".repeat(pad)), Style::DEFAULT));
                let room = (w as i64 - 5 - name_w as i64 - 2 - target_w as i64 - 3).max(1);
                // 子代理的行在行尾有数字列，名单的各行去掉同样的列（§4.2「子代理的数字」）；别的调用没有。
                let keep = if any_subagent {
                    subagent_cols::keep_for(room)
                } else {
                    0
                };
                let spans = match (&cells, keep) {
                    (Some(c), k) if k > 0 => {
                        let own =
                            room - subagent_cols::cols_width(k) as i64 - subagent_cols::GAP as i64;
                        let mut left = lead.clone();
                        left.extend(st.spans(&theme, own));
                        subagent_cols::right_at(&left, &subagent_cols::columns(c, k), w)
                    }
                    _ => {
                        let mut v = lead.clone();
                        v.extend(st.spans(&theme, room));
                        v
                    }
                };
                // 调用到达时整行直接出现：它是机器的输出（§4.2「笔」的「不是笔的东西」）。
                single_line(spans, false)
            });
            measured(row)
        })
        .collect();
    out.push(footer(&theme, batch, pen));
    out
}

/// 末行：`└`、点阵的四格与整批的计数：`2 running · 1 queued · 1 waiting for you · 3 done`，零不写。没有在跑的（只剩等你的
/// 调用），点阵与它的格子去掉：球在你这边时什么都不动。
// 以前只有多于 5 个调用时才有 `N more · M done` 这一行，平时最后一个调用的行是 `└`；
// 现在末行一直在：点阵在名单里有固定的一格，计数说的是整批（§4.2「一批调用」的「末行」）。
fn footer(theme: &Theme, batch: &[Call<'_>], pen: Option<&Pen>) -> Element {
    let out = batch.iter().filter(|m| running(m)).count();
    let count = |s: &str| {
        batch
            .iter()
            .filter(|m| !m.ended && tools::status(m) == s)
            .count()
    };
    let queued = count("queued");
    let asking = count("asking");
    let done = batch.iter().filter(|m| m.ended).count();
    let counts: Vec<Span> = [
        (out > 0).then(|| sp(format!("{out} running"), theme.fg2)),
        (queued > 0).then(|| sp(format!("{queued} queued"), theme.fg3)),
        (asking > 0).then(|| sp(format!("{asking} waiting for you"), Style::fg(theme.code))),
        (done > 0).then(|| sp(format!("{done} done"), theme.fg3)),
    ]
    .into_iter()
    .flatten()
    .collect();
    let mut spans = vec![
        sp("   ", Style::DEFAULT),
        sp("└", theme.line),
        sp(" ", Style::DEFAULT),
    ];
    if out > 0 {
        spans.extend(pen.map_or_else(pen::blank, Pen::slot));
    }
    let mut said = Vec::new();
    for (i, c) in counts.into_iter().enumerate() {
        if i > 0 {
            said.push(sp(" · ", theme.line));
        }
        said.push(c);
    }
    // 有在跑的调用时点阵在，第一段是 `N running`：点阵的光带接着走过它（§4.2「笔」的「手停住」的「光带」）。
    spans.extend(match pen {
        Some(p) if out > 0 => p.band(said),
        _ => said,
    });
    measured(single_line(spans, false))
}

/// 名册里一个调用此刻在做什么，算好的，画时再放进 `room` 格。
enum State {
    Queued,
    Asking,
    /// 结束了：标记与淡入的结果。
    Ended(Vec<Span>),
    /// 子代理此刻的调用：名字与对象。
    Calling(String, String),
    /// 命令的最后一行输出与在跑的时间。
    Tail(Option<String>, Vec<Span>),
}

impl State {
    fn of(ctx: &ToolCtx<'_>, m: &Call<'_>, clock: &ToolClock) -> State {
        let theme = ctx.theme;
        match tools::status(m) {
            "queued" => State::Queued,
            "asking" => State::Asking,
            s if m.ended => {
                // 结束：标记从 fg3 渐变到它的颜色，结果同时淡入。
                let result: Vec<(String, Style)> = tools::outcome(m, ctx.todos)
                    .into_iter()
                    .map(|p| (p.text, tool_row::ink(&theme, p.ink)))
                    .collect();
                let mut v = vec![tool_row::settled_mark(&theme, s, clock.ended, ctx.now)];
                if !result.is_empty() {
                    let parts = std::iter::once((" ".to_owned(), Style::DEFAULT))
                        .chain(result)
                        .collect();
                    v.extend(fade::parts(
                        &theme,
                        parts,
                        tool_row::age(clock.ended, ctx.now),
                        None,
                    ));
                }
                State::Ended(v)
            }
            _ => match tools::current(m) {
                // 子代理换了调用：新的直接换上。
                Some(c) => {
                    let (name, target) = tools::call_parts(c, ctx.where_);
                    State::Calling(name, target)
                }
                None => {
                    let live: Vec<Span> = tools::live(m, ctx.now_ms)
                        .iter()
                        .map(|p| tool_row::span(&theme, p))
                        .collect();
                    let last = match tools::under(m, false, ctx.where_, ctx.todos) {
                        Under::Tail(ls) => ls
                            .last()
                            .map(|l| jtrim(l).to_owned())
                            .filter(|l| !l.is_empty()),
                        _ => None,
                    };
                    State::Tail(last, live)
                }
            },
        }
    }

    fn spans(&self, theme: &Theme, room: i64) -> Vec<Span> {
        match self {
            State::Queued => vec![sp("queued", theme.fg3)],
            State::Asking => vec![sp("waiting for you", Style::fg(theme.code))],
            State::Ended(v) => fit::cut(v, room),
            State::Calling(name, target) => {
                let mut v = vec![sp(name.clone(), theme.fg2)];
                if !target.is_empty() {
                    v.push(sp(format!("  {target}"), theme.fg3));
                }
                fit::cut(&v, room)
            }
            State::Tail(Some(l), live) => {
                // 命令的最后一行输出用 quiet：它自己输出的 `✓` 不能读成我们的标记。
                let secs: Vec<Span> = if live.is_empty() {
                    Vec::new()
                } else {
                    std::iter::once(sp(" ", Style::DEFAULT))
                        .chain(live.iter().cloned())
                        .collect()
                };
                let mut v = fit::cut(
                    &[sp(l.clone(), theme.quiet)],
                    room - fit::width(&secs) as i64,
                );
                v.extend(secs);
                v
            }
            State::Tail(None, live) => fit::cut(live, room),
        }
    }
}
