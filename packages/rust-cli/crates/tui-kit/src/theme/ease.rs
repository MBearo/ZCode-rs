//! 笔（docs/puffin-tui.md §4.2「笔」）：代理的字整个落下，落定成它自己的墨；字下面什么都不涂。最新的字在
//! [`TICKS`] 个 tick 里从它的地升起，最多 [`CELLS`] 个；笔尖是一格。纯算术，输入是主题与铺好的行：笔在哪里由壳决定。
//! plain 模式没有落定。
//! 移植自 packages/tui/tui/src/zcode/tui/Ease.scala（feat/tui a2bf878b34）。
use std::borrow::Cow;

use crate::render::{Color, Span, Style, StyledLine, attr, width};

use super::rgb::Rgb;
use super::theme::Theme;

/// 一个字在这么多 tick 里落定（130 ms）。
pub const TICKS: i64 = 4;
/// 只有最新的这么多个字在落定；之前的都是实的。
pub const CELLS: usize = 16;
/// 新字从它的地向它的墨走了这么远处开始。
const FROM: f64 = 0.4;
/// 播放头不动这么多 tick 后笔尖停下（0.2 s）。
pub const REST_TICKS: i64 = 6;
/// 一轮结束时笔抬起用的 tick 数：`▌▍▎▏`。
pub const LIFT_TICKS: i64 = 4;
/// 笔抬起：一个 tick 一个字形。
pub const LIFT: [&str; 4] = ["▌", "▍", "▎", "▏"];
/// 笔尖，书写或停着。
pub const NIB: &str = "▌";

/// 谁的字：回答的（brand 的笔尖）或思考的（fg3 的笔尖）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Ink {
    Answer,
    Thought,
}

/// 落下 `age` 个 tick 的字从它的地向它的墨走了多远：0.4 升到 1，先快后慢；落定（或不计时，`age < 0`）是 1。
pub fn amount(age: i64) -> f64 {
    if !(0..TICKS).contains(&age) {
        return 1.0;
    }
    let p = (age + 1) as f64 / (TICKS + 1) as f64;
    FROM + (1.0 - FROM) * (1.0 - (1.0 - p) * (1.0 - p))
}

fn rgb(c: Color) -> Option<Rgb> {
    match c {
        Color::TrueColor(r, g, b) => Some(Rgb::new(r, g, b)),
        _ => None,
    }
}

/// `style` 落下 `age` 个 tick 后：它的墨从它的地混合出来（没有底色时是背景）；落定后或 plain 模式下原样。
pub fn settle(theme: &Theme, style: Style, age: i64) -> Style {
    let a = amount(age);
    if a >= 1.0 || !theme.rich {
        return style;
    }
    let ink = rgb(style.fg_color()).or(theme.fg);
    let ground = rgb(style.bg_color()).or(theme.bg);
    match (ink, ground) {
        (Some(i), Some(g)) => style.with_fg(g.mix(i, a).color()),
        _ => style,
    }
}

/// 笔尖的颜色：回答是 brand，思考是 fg3。
pub fn nib_color(theme: &Theme, ink: Ink) -> Color {
    match ink {
        Ink::Answer => theme.brand,
        Ink::Thought => theme.fg3.fg_color(),
    }
}

/// 抬起的笔：brand 向背景混合 25%。
pub fn lift_ink(theme: &Theme) -> Style {
    match (rgb(theme.brand), theme.bg) {
        (Some(b), Some(g)) => Style::fg(b.mix(g, 0.25).color()),
        _ => Style::fg(theme.brand),
    }
}

// ---- 涂铺好的行 ----

/// `lines` 里最新写出的格子（[`attr::WRITTEN`]）在落定：第 `k` 新的、不是空白的格子（从 1 起）落下了 `age_of(k)`
/// 个 tick。最多数 [`CELLS`] 个；没有写出标记的格子与空白保持原样、也不计数。plain 模式：行原样返回。
pub fn settled(
    theme: &Theme,
    age_of: impl Fn(usize) -> i64,
    mut lines: Vec<StyledLine>,
) -> Vec<StyledLine> {
    if !theme.rich {
        return lines;
    }
    let mut k = 0usize;
    for line in lines.iter_mut().rev() {
        if k >= CELLS {
            break;
        }
        if !line.spans.iter().any(|sp| sp.style.has_attr(attr::WRITTEN)) {
            continue;
        }
        // 按格子从右往左数；同一个 span 里的字可能落下的时刻不同，所以拆到格子（码点）。
        let mut cells: Vec<Span> = line
            .spans
            .iter()
            .flat_map(|sp| {
                sp.text.chars().map(|c| {
                    Span::with_decor(Cow::Owned(c.to_string()), sp.style, sp.decor.clone())
                })
            })
            .collect();
        let mut changed = false;
        for cell in cells.iter_mut().rev() {
            if k >= CELLS {
                break;
            }
            if cell.style.has_attr(attr::WRITTEN)
                && cell.text.chars().any(|c| !java_space(c))
                && width::string_width(&cell.text) > 0
            {
                k += 1;
                let st = settle(theme, cell.style, age_of(k));
                if st != cell.style {
                    cell.style = st;
                    changed = true;
                }
            }
        }
        if changed {
            line.spans = merge(cells);
        }
    }
    lines
}

/// 代理写出的、整个落下的东西（表格）进入 `age` 个 tick：每个格子一起落定。plain 模式与落定之后原样。
pub fn landed(theme: &Theme, age: i64, mut lines: Vec<StyledLine>) -> Vec<StyledLine> {
    if !theme.rich || amount(age) >= 1.0 {
        return lines;
    }
    for sp in lines.iter_mut().flat_map(|l| l.spans.iter_mut()) {
        sp.style = settle(theme, sp.style, age);
    }
    lines
}

/// 相邻、样式与 decor 都相同的段并成一段。
fn merge(spans: Vec<Span>) -> Vec<Span> {
    let mut out: Vec<Span> = Vec::with_capacity(spans.len());
    for sp in spans {
        match out.last_mut() {
            Some(last) if last.style == sp.style && last.decor == sp.decor => {
                last.text.to_mut().push_str(&sp.text);
            }
            _ => out.push(sp),
        }
    }
    out
}

/// Java 的 `Character.isWhitespace`（Scala 的 `isWhitespace`）：Unicode 的空白，但不含不折行的空格（U+00A0、
/// U+2007、U+202F）与 U+0085，另加 U+001C–U+001F。与 Rust 的 `char::is_whitespace` 不同，计数要与 Scala 逐格相同。
pub fn java_space(c: char) -> bool {
    match c {
        '\u{1c}'..='\u{1f}' => true,
        '\u{85}' | '\u{a0}' | '\u{2007}' | '\u{202f}' => false,
        _ => c.is_whitespace(),
    }
}
