//! 竖向裁剪（docs/puffin-tui.md §4.4）：把铺好的行限在最多 `max(max_rows, 3)` 行，标记算在内。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Layout.scala 的 `layClipped` / `clipMarker`（feat/tui a2bf878b34）。
//!
//! [`ClipFocus::Cursor`] 把窗口居中在第一个含 [`attr::CARET`] 样式段的行（输入框的光标格）上，找不到时退到最后一行。
//! 贴边的两种窗口都只有一个标记，留下的每一行都与它的边相连。
use crate::render::{Span, StyledLine, attr, width};

use super::super::element::{Clip, ClipFocus};

pub(super) fn lay_clipped(mut rows: Vec<StyledLine>, c: &Clip, width: usize) -> Vec<StyledLine> {
    let h = c.max_rows.max(3);
    let total = rows.len();
    if total <= h {
        return rows;
    }
    let marker = |hidden: i64| clip_marker(hidden, c, width);
    let count = |n: usize| n as i64;
    match c.focus {
        ClipFocus::Head => {
            rows.truncate(h - 1);
            rows.push(marker(count(total - (h - 1))));
            rows
        }
        ClipFocus::Ends { head, tail } => {
            // Scala 在 head + tail 超过总行数时给出负数的标记，并让两头重叠：照样保留。
            let hidden = count(total) - count(head) - count(tail);
            let mut out: Vec<StyledLine> = rows[..head.min(total)].to_vec();
            out.push(marker(hidden));
            out.extend_from_slice(&rows[total.saturating_sub(tail)..]);
            out
        }
        ClipFocus::Cursor => {
            let fi = rows
                .iter()
                .position(|l| l.spans.iter().any(|sp| sp.style.has_attr(attr::CARET)))
                .unwrap_or(total - 1);
            // 让光标行居中在 `h - 2` 行内容（上下各一个标记）里的窗口顶。到边时一个标记就够，空出它的一行给内容。
            let a0 = count(fi) - count((h - 3) / 2);
            if a0 <= 0 {
                rows.truncate(h - 1);
                rows.push(marker(count(total - (h - 1))));
                rows
            } else if a0 as usize + (h - 2) >= total {
                let mut out = vec![marker(count(total - (h - 1)))];
                out.extend(fresh(rows.split_off(total - (h - 1))));
                out
            } else {
                let a0 = a0 as usize;
                let mut out = vec![marker(count(a0))];
                out.extend(fresh(rows[a0..a0 + (h - 2)].to_vec()));
                out.push(marker(count(total - a0 - (h - 2))));
                out
            }
        }
    }
}

/// 标记之后的第一行不再接着它折出来的那一行。
fn fresh(mut kept: Vec<StyledLine>) -> Vec<StyledLine> {
    if let Some(first) = kept.first_mut() {
        first.cont = None;
    }
    kept
}

/// `lead` 之后一行 `ink` 的 `… N more lines`（有提示时接 ` (hint)`），像横线的标签一样截到铺的宽度。Scala 按 UTF-16
/// 单元截（不是按宽度），这里按码点截。
fn clip_marker(hidden: i64, c: &Clip, width: usize) -> StyledLine {
    let mut text = (c.more.0)(hidden);
    if !c.hint.is_empty() {
        text.push_str(&format!(" ({})", c.hint));
    }
    let lead_w: usize = c.lead.iter().map(|sp| width::string_width(&sp.text)).sum();
    let room = width.saturating_sub(lead_w).max(1);
    let mut spans = c.lead.clone();
    spans.push(Span::new(
        text.chars().take(room).collect::<String>(),
        c.ink,
    ));
    StyledLine::new(spans)
}
