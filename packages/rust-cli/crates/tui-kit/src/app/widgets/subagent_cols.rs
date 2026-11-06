//! 子代理的数字的列（docs/puffin-tui.md §4.2「子代理的数字」）：时间、token 与速度三列，在代表一个子代理的每一行的末尾。
//! 移植自 Puffin 的 shell/subagents.rs 的列的纯函数部分（Subagents.scala）；每格的字（[`Cells`]）由界面按它
//! 知道的数字算好交进来。
use crate::render::{Span, Style};

use super::fit;

/// 列宽：时间、token、速度；之间两格。
pub const WIDTHS: [usize; 3] = [6, 9, 9];
pub const GAP: usize = 2;
/// 行自己的字至少留这么多格（§4.2「子代理的数字」的「窄的时候」）。
pub const OWN_MIN: i64 = 24;

/// 前 `keep` 列占的格数。
pub fn cols_width(keep: usize) -> usize {
    if keep == 0 {
        0
    } else {
        WIDTHS.iter().take(keep).sum::<usize>() + GAP * (keep - 1)
    }
}

/// 行给自己的字与列留了 `room` 格时放得下几列：先去速度，再去 token；时间放得下就留。
pub fn keep_for(room: i64) -> usize {
    (1..=3)
        .rev()
        .find(|k| room >= cols_width(*k) as i64 + OWN_MIN)
        .unwrap_or(0)
}

/// 三格的字；空的一格也占着它的宽度。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Cells {
    pub time: Vec<Span>,
    pub tokens: Vec<Span>,
    pub speed: Vec<Span>,
}

/// 前 `keep` 格，每格在它的宽度里右对齐。
pub fn columns(c: &Cells, keep: usize) -> Vec<Span> {
    let mut out = Vec::new();
    for (i, (spans, w)) in [&c.time, &c.tokens, &c.speed]
        .into_iter()
        .zip(WIDTHS)
        .take(keep)
        .enumerate()
    {
        if i > 0 {
            out.push(Span::new(" ".repeat(GAP), Style::DEFAULT));
        }
        let pad = w.saturating_sub(fit::width(spans));
        if pad > 0 {
            out.push(Span::new(" ".repeat(pad), Style::DEFAULT));
        }
        out.extend(spans.iter().cloned());
    }
    out
}

/// `left`，然后 `right` 结束在第 `edge` 格：`left` 用 `…` 截断，与 `right` 之间留两格。
pub fn right_at(left: &[Span], right: &[Span], edge: usize) -> Vec<Span> {
    if right.is_empty() {
        return fit::cut(left, edge as i64);
    }
    let rw = fit::width(right) as i64;
    let mut l = fit::cut(left, (edge as i64 - rw - GAP as i64).max(0));
    let fill = (edge as i64 - fit::width(&l) as i64 - rw).max(GAP as i64) as usize;
    l.push(Span::new(" ".repeat(fill), Style::DEFAULT));
    l.extend(right.iter().cloned());
    l
}
