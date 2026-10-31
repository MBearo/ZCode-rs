// 复制自 apps/zcode-host/crates/tui/src/ui/logo/mod.rs（2f982570bb）；只改了 spec 的出处（docs/specs/rust-cli/tui.md §7.3）。
//! 新会话的背景标志（docs/specs/rust-cli/tui.md §7.3；原为 rust-tui.md §8.11）：Z 镂空的立体金属徽章。平时是淡色的背景；点击时按点击的
//! 位置被“按”进去一下，弹簧回弹，金属的反射灯条随倾斜在表面滑动。
mod render;
mod shape;

use std::f64::consts::TAU;
use std::time::Instant;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;

use super::palette::{Palette, Rgb, blend};
pub use render::{Cell, Metal, Pose, REST, frame};

/// 一次点击的时长（秒）：0.1 秒亮起，弹簧在 1.2 秒内停住，`FADE_FROM` 起淡回。
pub const DURATION: f64 = 2.0;
const RISE: f64 = 0.1;
const FADE_FROM: f64 = 1.3;
/// 平时的不透明度。
pub const STATIC_OPACITY: f64 = 0.18;
/// 按在徽章边缘时的最大倾斜（弧度）与按在中间时的最大下沉。
const MAX_TILT: f64 = 0.42;
const MAX_PUSH: f64 = 0.12;
/// 标志最宽 60 列，高:宽 = 21:60，至少 14 行。
const MAX_COLUMNS: u16 = 60;
const RATIO_ROWS: u16 = 21;
const MIN_ROWS: u16 = 14;
/// 查不到终端背景色时按这个深色背景混合。
const DARK: Rgb = (0x1e, 0x1e, 0x2e);
/// smootherstep `t³(t(6t − 15) + 10)` 的三个系数。
const SMOOTHERSTEP: [f64; 3] = [6.0, 15.0, 10.0];
/// 按下的弹簧：振幅、衰减的时间常数（秒）、频率（次 / 秒）。
const SPRING_AMPLITUDE: f64 = 1.6;
const SPRING_DECAY: f64 = 0.32;
const SPRING_FREQUENCY: f64 = 1.5;
/// 标志左右合计留出的列数。
const SIDE_MARGIN: u16 = 4;
/// 字符格的中心（相对左上角）。
const CELL_CENTER: f64 = 0.5;
/// 盲文点阵字符的起点（U+2800，没有点）。
const BRAILLE_BLANK: u32 = 0x2800;
/// 16 色终端不透明度低于它时用 dim。
const DIM_BELOW: f64 = 0.5;

fn smooth(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    let [a, b, c] = SMOOTHERSTEP;
    t * t * t * (t * (t * a - b) + c)
}

/// 按下深度：衰减的弹簧，约 0.14 秒按到最深（≈1），回弹时轻微过冲，1.2 秒内停住。
pub fn depth(t: f64) -> f64 {
    if t <= 0.0 {
        return 0.0;
    }
    SPRING_AMPLITUDE * (-t / SPRING_DECAY).exp() * (TAU * SPRING_FREQUENCY * t).sin()
}

/// 点击 `point`（徽章上的位置，-1..1）后 `t` 秒的姿态：点到的一侧往里，整体下沉，越靠中间沉得越多。
pub fn pose_at(point: [f64; 2], t: f64) -> Pose {
    let d = depth(t);
    let offset = point[0].hypot(point[1]).min(1.0);
    Pose {
        yaw: MAX_TILT * point[0] * d,
        pitch: -MAX_TILT * point[1] * d,
        // 按在边缘时只沉一半。
        push: -MAX_PUSH * d * (1.0 - offset / 2.0),
    }
}

/// 点击后 `t` 秒的不透明度。
pub fn opacity_at(t: f64) -> f64 {
    if t < RISE {
        STATIC_OPACITY + (1.0 - STATIC_OPACITY) * smooth(t / RISE)
    } else if t < FADE_FROM {
        1.0
    } else if t >= DURATION {
        STATIC_OPACITY
    } else {
        1.0 + (STATIC_OPACITY - 1.0) * smooth((t - FADE_FROM) / (DURATION - FADE_FROM))
    }
}

/// 空白处里标志的位置：居中，放不下时为 None。
pub fn stage_in(free: Rect) -> Option<Rect> {
    let width = free
        .width
        .saturating_sub(SIDE_MARGIN)
        .min(MAX_COLUMNS)
        .min(free.height.saturating_mul(MAX_COLUMNS) / RATIO_ROWS);
    let height = width * RATIO_ROWS / MAX_COLUMNS;
    (height >= MIN_ROWS).then(|| {
        Rect::new(
            free.x + (free.width - width) / 2,
            free.y + (free.height - height) / 2,
            width,
            height,
        )
    })
}

/// 屏幕上的字符格 → 徽章上的位置（-1..1）。
pub fn to_badge(stage: Rect, column: u16, row: u16) -> [f64; 2] {
    render::badge_point(
        stage.width.into(),
        stage.height.into(),
        f64::from(column.saturating_sub(stage.x)) + CELL_CENTER,
        f64::from(row.saturating_sub(stage.y)) + CELL_CENTER,
    )
}

#[derive(Debug, Default)]
pub struct Logo {
    /// 最近一次点击：时间与徽章上的位置。
    pressed: Option<(Instant, [f64; 2])>,
    /// 这一帧画在哪里（点击的目标）；没画时为 None。
    pub stage: Option<Rect>,
}

impl Logo {
    /// 需要定时重绘：正在画，并且点击的效果还没结束。
    pub fn animating(&self, now: Instant) -> bool {
        self.stage.is_some()
            && self
                .pressed
                .is_some_and(|(at, _)| now.duration_since(at).as_secs_f64() < DURATION)
    }

    /// 在 `(column, row)` 按下：从这个位置重新开始一次按压（没画时不算）。
    pub fn press(&mut self, column: u16, row: u16, now: Instant) {
        if let Some(stage) = self.stage {
            self.pressed = Some((now, to_badge(stage, column, row)));
        }
    }

    /// 这一帧不画：正在进行的按压结束。
    pub fn hide(&mut self) {
        self.stage = None;
        self.pressed = None;
    }

    pub fn draw(&mut self, buffer: &mut Buffer, free: Rect, palette: &Palette, now: Instant) {
        let Some(stage) = stage_in(free.intersection(buffer.area)) else {
            self.hide();
            return;
        };
        self.stage = Some(stage);
        let (pose, opacity) = match self.pressed {
            Some((at, point)) if now.duration_since(at).as_secs_f64() < DURATION => {
                let t = now.duration_since(at).as_secs_f64();
                (pose_at(point, t), opacity_at(t))
            }
            _ => {
                self.pressed = None;
                (REST, STATIC_OPACITY)
            }
        };
        let background = palette.background.unwrap_or(DARK);
        let metal = Metal::new(palette.light);
        let cells = frame(stage.width.into(), stage.height.into(), pose, &metal);
        let width = usize::from(stage.width);
        for (index, cell) in cells.iter().enumerate().filter(|(_, cell)| cell.dots != 0) {
            let x = stage.x + (index % width) as u16;
            let y = stage.y + (index / width) as u16;
            let target = &mut buffer[(x, y)];
            target.set_char(char::from_u32(BRAILLE_BLANK + u32::from(cell.dots)).unwrap_or(' '));
            match palette.tint(blend(background, cell.rgb, opacity)) {
                Some(color) => {
                    target.set_fg(color);
                }
                // 16 色终端不上色：淡的时候用 dim。
                None if opacity < DIM_BELOW => target.modifier.insert(Modifier::DIM),
                None => {}
            }
        }
    }
}

#[cfg(test)]
#[path = "logo_tests.rs"]
mod tests;
