//! 一段读、搜索与抓取折成一行（docs/puffin-tui.md §4.2「工具调用」的「读的折叠」）：开着时 `Exploring` 与最新调用的
//! 跑马灯，收拢后 `Explored` 与计数；^O 时下面是每个调用。
//! 移植自 Puffin 的 shell/tool_view.rs 的 `run` … `swap`（ToolView.scala）。
use crate::app::element::{Element, beside, by_width, single_line, stack};
use crate::markdown::render::SETTLED;
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::super::fade;
use super::super::fit::{self, measured};
use super::super::tools::{self, Call};
use super::{PAST, ToolClock, ToolCtx, call_row, gap, rail, ring, settled, sp, span, under};

fn reads_of<'a>(members: &[Call<'a>]) -> Vec<Call<'a>> {
    members
        .iter()
        .filter(|m| !tools::hidden(tools::tool(m)))
        .copied()
        .collect()
}

/// 一段读、搜索与抓取作为一行。开着时是 `Exploring` 与最新调用的跑马灯；收拢后是 `Explored` 与计数。只有一次调用的、收拢
/// 的一段就是那次调用自己的行。`clocks` 给每个调用（按 id）的动效；`closed` 是这段结束的时刻。
pub fn run(
    ctx: &ToolCtx<'_>,
    members: &[Call<'_>],
    clocks: &dyn Fn(&str) -> ToolClock,
    closed: Option<i64>,
) -> Element {
    let theme = ctx.theme;
    let Some(first) = members.first() else {
        return Element::Blank;
    };
    let reads = reads_of(members);
    let head = clocks(first.id);
    let settle_at = settle_tick(members, clocks, closed);
    // 收拢后只有一次调用的折叠就是那次调用自己的行，下面不再重复它（§4.2「工具调用」的「折叠里的调用」）。
    let one = settle_at.is_some() && reads.len() == 1;
    let details = if ctx.details && !one {
        member_rows(ctx, members)
    } else {
        Vec::new()
    };
    let mut rows: Vec<Element> = if one {
        let only = &reads[0];
        let c = clocks(only.id);
        let shifted = ToolClock {
            arrived: None,
            ended: settle_at.filter(|_| c.ended.is_some()),
            ..c.clone()
        };
        let mut v = vec![call_row(ctx, only, &shifted, None)];
        v.extend(under(ctx, only, &c));
        v
    } else {
        let label = sp(
            if settle_at.is_some() {
                "Explored"
            } else {
                "Exploring"
            },
            theme.fg2.with_attr(attr::BOLD),
        );
        match settle_at {
            None => {
                let Some(cur) = reads.last().or(members.last()) else {
                    return Element::Blank;
                };
                let prev = reads.len() >= 2;
                let mark = sp("○", ring(&theme, head.arrived, ctx.now, theme.fg3));
                // 第一次调用：整行一起淡入；之后每次调用换上新的标签并淡入。
                let swapped = if prev {
                    clocks(cur.id).arrived.unwrap_or(PAST)
                } else {
                    PAST
                };
                let text = tools::label(cur, ctx.where_, ctx.tasks, ctx.todos);
                let swap_age = ctx.now.saturating_sub(swapped);
                let fg3 = theme.fg3;
                vec![explore_row(
                    &theme,
                    mark,
                    label,
                    head.arrived.map(|t| ctx.now - t),
                    move |w| swap(&theme, &text, swap_age, fg3, w),
                    Vec::new(),
                )]
            }
            Some(at) => {
                let failed = reads.iter().filter(|m| tools::failed(m)).count();
                let mark = sp(
                    "✓",
                    fade::ink(
                        &theme,
                        theme.fg3.fg_color(),
                        Style::fg(theme.ok),
                        ctx.now - at,
                        fade::TICKS,
                    ),
                );
                let after = if failed == 0 {
                    Vec::new()
                } else {
                    vec![gap(2), sp(format!("{failed} failed"), Style::fg(theme.err))]
                };
                let text = tools::counts(&reads);
                let age = ctx.now - at;
                let fg3 = theme.fg3;
                vec![explore_row(
                    &theme,
                    mark,
                    label,
                    None,
                    move |w| swap(&theme, &text, age, fg3, w),
                    after,
                )]
            }
        }
    };
    rows.extend(details);
    stack(rows)
}

/// 一段收拢的 tick：它结束或它最后一个调用结束，取晚的；开着时没有。
pub fn settle_tick(
    members: &[Call<'_>],
    clocks: &dyn Fn(&str) -> ToolClock,
    closed: Option<i64>,
) -> Option<i64> {
    closed.filter(|_| members.iter().all(|m| m.ended)).map(|c| {
        members
            .iter()
            .filter_map(|m| clocks(m.id).ended)
            .fold(c, i64::max)
    })
}

/// 一段的动效结束了。
pub fn run_settled(
    ctx: &ToolCtx<'_>,
    members: &[Call<'_>],
    clocks: &dyn Fn(&str) -> ToolClock,
    closed: Option<i64>,
) -> bool {
    settle_tick(members, clocks, closed).is_some_and(|at| {
        let reads = reads_of(members);
        ctx.now - at >= fade::TICKS
            && (reads.len() != 1 || settled(ctx, &reads[0], &clocks(reads[0].id)))
    })
}

fn explore_row(
    theme: &Theme,
    mark: Span,
    label: Span,
    arrived: Option<i64>,
    ticker: impl Fn(usize) -> Vec<Span> + Send + Sync + 'static,
    after: Vec<Span>,
) -> Element {
    let lead_width = 3 + string_width(&label.text) + 2;
    let mut lead = vec![gap(1), mark, gap(1)];
    lead.extend(fade::fade_in(
        theme,
        vec![label],
        arrived.unwrap_or(SETTLED),
        fade::TICKS,
        None,
    ));
    lead.push(gap(2));
    let after_width = fit::width(&after);
    measured(beside(
        single_line(lead, false),
        lead_width,
        by_width(move |w| {
            let mut spans = ticker(w.saturating_sub(after_width).max(1));
            spans.extend(after.iter().cloned());
            single_line(spans, false)
        }),
    ))
}

/// ^O 打开时一段下面的：每个调用的标记、名称、对象与结果。
fn member_rows(ctx: &ToolCtx<'_>, members: &[Call<'_>]) -> Vec<Element> {
    let theme = ctx.theme;
    let nw = members
        .iter()
        .map(|m| string_width(&tools::name(m)))
        .max()
        .unwrap_or(0)
        + 2;
    members
        .iter()
        .map(|m| {
            let status = tools::status(m);
            let queued = status == "queued";
            let mark = match status {
                "done" => sp("✓ ", Style::fg(theme.ok)),
                "error" => sp("✗ ", Style::fg(theme.err)),
                "cancelled" => sp("– ", theme.fg3),
                "asking" => sp("● ", Style::fg(theme.code)),
                "queued" => sp("○ ", theme.line),
                _ => sp("○ ", theme.fg3),
            };
            let end: Vec<Span> = if m.ended {
                tools::outcome(m, ctx.todos)
                    .iter()
                    .map(|p| span(&theme, p))
                    .collect()
            } else if queued {
                vec![sp("queued", theme.fg3)]
            } else {
                Vec::new()
            };
            let end_w = fit::width(&end) as i64;
            let target = tools::target(m, ctx.where_, ctx.tasks, ctx.todos);
            let name = sp(tools::name(m), if queued { theme.fg3 } else { theme.fg2 });
            let writing = tools::writing(m);
            measured(by_width(move |w| {
                let room = w as i64 - 5 - 2 - nw as i64 - if end_w > 0 { end_w + 2 } else { 0 };
                let shown = if string_width(&target) as i64 <= room {
                    target.clone()
                } else if room >= 4 {
                    fit::cut_text(&target, room as usize)
                } else {
                    String::new()
                };
                let mut spans = rail(&theme);
                // 输入还在写出：只有名称（§4.2「笔」的「调用」）。
                if writing {
                    spans.push(mark.clone());
                    spans.push(name.clone());
                } else {
                    spans.push(mark.clone());
                    spans.push(sp(fit::pad(&name.text, nw), name.style));
                    spans.push(sp(shown, theme.fg3));
                    if end_w > 0 {
                        spans.push(gap(2));
                        spans.extend(end.iter().cloned());
                    }
                }
                single_line(spans, false)
            }))
        })
        .collect()
}

/// `w` 格宽的一行上的跑马灯：`cur` 立刻换掉那里原有的，从背景升到 `ink`。`age` 是换上以来的 tick。
pub fn swap(theme: &Theme, cur: &str, age: i64, ink: Style, w: usize) -> Vec<Span> {
    fade::fade_in(
        theme,
        vec![sp(fit::clip(cur, w), ink)],
        age,
        fade::LAND_TICKS,
        None,
    )
}
