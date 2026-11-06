//! 新会话的背景标志（docs/specs/rust-cli/tui.md §7.3）：品牌 Z 用盲文的点画在 transcript 的空白处，平时很淡；点在 Z 上时变亮，
//! 像一块弹簧板那样被按下，动着时再点，弹簧相加。它不是 transcript 的行：`draw.rs` 把它盖在排好的空行上，于是选区与悬停都
//! 碰不到它，开着的层照常盖住它。
mod model;
mod press;
mod render;
mod shape;

use std::time::Instant;

use rust_cli_tui_kit::render::{Color, Span, Style, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

use super::frame::overlay;
use model::Model;
use press::{Press, smooth};
use render::{Canvas, Cell, Inks, Rgb, mix};
use shape::{ACROSS, DOWN};

// 同事反馈以前（0.8、最多 22 行）太大，压到一半、最多 12 行；6 行高时 Z 还认得出。
/// 比这矮不画。
const MIN_ROWS: usize = 6;
const MAX_ROWS: usize = 12;
/// Z 占空白处的比例。
const FILL: f64 = 0.5;
/// 左右至少共留这么多列。
const SIDE_MARGIN: usize = 4;
/// 点中要求的覆盖率（一格 8 个点的平均）。
const HIT_COVER: f64 = 0.4;
/// 静止的点从背景向 fg3 走这么多（Faint）。
const REST: f64 = 0.42;
/// 点亮后的颜色（Sea）：从背景向前景走 `PEAK_LIGHT`，再向 brand 走 `PEAK_BRAND`。
const PEAK_LIGHT: f64 = 0.88;
const PEAK_BRAND: f64 = 0.45;
/// 显著度：点下后升起、最后一次按压结束后停住、再落回的时长（秒）。
const RISE: f64 = 0.18;
const HOLD: f64 = 0.3;
const FALL: f64 = 0.9;

fn rgb(color: Color) -> Option<Rgb> {
    match color {
        Color::TrueColor(r, g, b) => Some([f64::from(r), f64::from(g), f64::from(b)]),
        _ => None,
    }
}

fn inks(theme: &Theme) -> Option<Inks> {
    let tone =
        |c: rust_cli_tui_kit::theme::rgb::Rgb| [f64::from(c.r), f64::from(c.g), f64::from(c.b)];
    let bg = tone(theme.bg.filter(|_| theme.rich)?);
    let fg = tone(theme.fg?);
    let fg3 = rgb(theme.fg3.fg_color())?;
    let brand = rgb(theme.brand)?;
    // Sea：亮的时候向启动头里蓝色的 Z 走。
    Some(Inks {
        bg,
        rest: mix(bg, fg3, REST),
        peak: mix(mix(bg, fg, PEAK_LIGHT), brand, PEAK_BRAND),
    })
}

/// 这一帧标志画在哪里。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Stage {
    /// Z 外框左上的格子与 Z 的行数。
    pub col: usize,
    pub row: usize,
    pub rows: usize,
    /// 屏幕上第几行之前看得见（下面的被开着的层盖住，点了不算）。
    pub shown: usize,
    /// 画布：空白处的第一行、行数与屏幕的列数（翘起的一边可以画到 Z 的外框之外）。
    top: usize,
    free: usize,
    cols: usize,
    inks: Inks,
}

impl Stage {
    fn canvas(&self) -> Canvas {
        Canvas {
            width: self.cols * ACROSS,
            height: self.free * DOWN,
            left: (self.col * ACROSS) as isize,
            top: ((self.row - self.top) * DOWN) as isize,
        }
    }
}

/// 与 Scala / JS 的 round 相同：.5 向上；负数取 0。
fn round(v: f64) -> usize {
    render::round(v).max(0.0) as usize
}

/// Z 宽多少列（不取整）：`rows` 行高时。
fn columns(rows: usize) -> f64 {
    shape::width_of(rows * DOWN) / ACROSS as f64
}

/// `top` 是空白处的第一行（最后一行文字之后空一行），`body` 是正文按没有层的一摞量的高度：最后留一行空行。放不下时 `None`。
pub fn stage(theme: &Theme, cols: usize, top: usize, body: usize) -> Option<Stage> {
    let inks = inks(theme)?;
    let free = body.checked_sub(top + 1)?;
    // 设计稿只按高度定大小；窄终端上 Z 会被裁掉，所以再按宽度限制。
    let fits = (cols.saturating_sub(SIDE_MARGIN) as f64 / columns(1)).floor() as usize;
    let rows = round(free as f64 * FILL).min(MAX_ROWS).min(fits);
    (rows >= MIN_ROWS).then(|| Stage {
        col: round((cols as f64 - columns(rows)) / 2.0),
        row: top + (free - rows) / 2,
        rows,
        shown: body,
        top,
        free,
        cols,
        inks,
    })
}

#[derive(Default)]
pub struct Mark {
    /// 还在动的按压。
    presses: Vec<Press>,
    /// 这一串相连的按压的开始与结束，以及开始时的显著度（还没点过时没有）。
    chain: Option<(f64, f64, f64)>,
    epoch: Option<Instant>,
    /// 模型只按行数变，记下最近的一份。
    model: Option<Model>,
}

/// `rows` 行高的模型：`slot` 里的那份行数不对时重建。
fn cached(slot: &mut Option<Model>, rows: usize) -> &Model {
    if slot.as_ref().is_some_and(|m| m.dots.rows != rows) {
        *slot = None;
    }
    slot.get_or_insert_with(|| Model::new(rows))
}

impl Mark {
    fn secs(&self, now: Instant) -> f64 {
        self.epoch.map_or(0.0, |epoch| {
            now.saturating_duration_since(epoch).as_secs_f64()
        })
    }

    /// 显著度（0 静止，1 点亮）。
    fn salience(&self, at: f64) -> f64 {
        let Some((start, end, from)) = self.chain else {
            return 0.0;
        };
        if at < end {
            from + (1.0 - from) * smooth((at - start) / RISE)
        } else {
            1.0 - smooth((at - end - HOLD) / FALL)
        }
    }

    /// 还在动或显著度还没落回（`App::animating`：只在这时按动画节拍重画）。
    pub fn moving(&self, now: Instant) -> bool {
        self.chain
            .is_some_and(|(_, end, _)| self.secs(now) < end + HOLD + FALL)
    }

    /// 左键按在 `(x, y)`：点在 Z 上时按下去（立刻开始，与还在动的按压相加），返回 true。
    pub fn press(&mut self, stage: &Stage, x: u16, y: u16, now: Instant) -> bool {
        let (x, y) = (usize::from(x), usize::from(y));
        if x < stage.col || y < stage.row || y >= stage.shown {
            return false;
        }
        let (bx, by) = (
            ((x - stage.col) * ACROSS) as isize,
            ((y - stage.row) * DOWN) as isize,
        );
        let dots = &cached(&mut self.model, stage.rows).dots;
        let cover: f64 = (0..DOWN as isize)
            .flat_map(|dy| (0..ACROSS as isize).map(move |dx| (dx, dy)))
            .map(|(dx, dy)| dots.cover(bx + dx, by + dy))
            .sum();
        if cover / (ACROSS * DOWN) as f64 <= HIT_COVER {
            return false;
        }
        let epoch = *self.epoch.get_or_insert(now);
        let at = now.saturating_duration_since(epoch).as_secs_f64();
        self.presses.retain(|p| p.end() > at);
        // 没有在动的按压时开始新的一串：显著度从当时的值升起；否则这一串延长到这次按压结束。
        let (first, from) = match (self.presses.is_empty(), self.chain) {
            (false, Some((first, _, from))) => (first, from),
            _ => (at, self.salience(at)),
        };
        let press = Press {
            start: at,
            point: [
                bx as f64 + ACROSS as f64 / 2.0,
                by as f64 + DOWN as f64 / 2.0,
            ],
        };
        self.presses.push(press);
        self.chain = Some((first, press.end(), from));
        true
    }

    /// 这一刻的格子：`stage` 的画布上，行优先。
    fn cells(&mut self, stage: &Stage, now: Instant) -> Vec<Option<Cell>> {
        let at = self.secs(now);
        let sal = self.salience(at);
        let canvas = stage.canvas();
        let model = cached(&mut self.model, stage.rows);
        let pose = self
            .chain
            .and_then(|(first, last, _)| press::pose(model, &self.presses, (first, last), at));
        let frame = render::dots(model, canvas, pose);
        render::cells(&frame, canvas, &stage.inks, sal)
    }

    /// 把 Z 画在 `lines`（屏幕的行，空白处是空的）上：一格一个盲文字符，只设前景色。
    pub fn paint(&mut self, lines: &mut [StyledLine], stage: &Stage, now: Instant) {
        let cells = self.cells(stage, now);
        for (r, row) in cells.chunks(stage.cols).enumerate() {
            let Some(line) = lines.get_mut(stage.top + r) else {
                break;
            };
            let (Some(first), Some(last)) = (
                row.iter().position(Option::is_some),
                row.iter().rposition(Option::is_some),
            ) else {
                continue;
            };
            let mut spans: Vec<(String, Style)> = Vec::new();
            for cell in &row[first..=last] {
                let (glyph, style) = match cell {
                    Some(Cell {
                        glyph,
                        rgb: [r, g, b],
                    }) => (*glyph, Style::fg(Color::TrueColor(*r, *g, *b))),
                    None => (' ', Style::DEFAULT),
                };
                match spans.last_mut() {
                    Some((text, last)) if *last == style => text.push(glyph),
                    _ => spans.push((glyph.to_string(), style)),
                }
            }
            let piece = StyledLine::new(
                spans
                    .into_iter()
                    .map(|(text, style)| Span::new(text, style))
                    .collect(),
            );
            *line = overlay(line, first, piece);
        }
    }
}

#[cfg(test)]
#[path = "logo_tests.rs"]
mod tests;
