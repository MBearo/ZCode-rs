// 复制自 apps/zcode-host/crates/tui/src/ui/logo/render.rs（2f982570bb）；只改了 spec 的出处。
//! 金属徽章的投影与点阵（docs/specs/rust-cli/tui.md §7.3；原为 rust-tui.md §8.11）：距离场挤出成带倒角、正面微微隆起的实体，按姿态
//! 旋转、后退，透视投影到盲文点阵（每个字符 2×4 个点），深度缓冲只留最前面的点；每个点按反射方向在固定的
//! “摄影棚”环境里取色。
use std::f64::consts::FRAC_1_SQRT_2;

use super::shape::{EXTENT, FIELD, GRID, STEP};
use crate::ui::palette::{Rgb, blend};

/// 盲文字符里 (列, 行) 点的位：U+2800 + 位的和。
const DOT_BITS: [[u8; 4]; 2] = [[0x01, 0x02, 0x04, 0x40], [0x08, 0x10, 0x20, 0x80]];
/// 实体的半厚度与倒角宽度（距离场的单位）。
const HALF_DEPTH: f64 = 0.15;
const BEVEL: f64 = 0.07;
/// 正面中间隆起的程度：法线按位置向外偏，反射的灯条因此落在面上的一处，而不是整面一样亮。
const DOME: f64 = 0.22;
/// 透视的观察距离。
const VIEW: f64 = 4.0;
/// 一个盲文字符的点：2 列、4 行。
const DOT_COLUMNS: usize = DOT_BITS.len();
const DOT_ROWS: usize = DOT_BITS[0].len();
/// 按短边缩放时留给透视放大的余量（只占短边的 95%）。
const FIT: f64 = 0.95;
/// 反射里的两条灯条：沿对角线的位置（偏移）、宽度；副灯条的亮度。
const MAIN_STRIP_OFFSET: f64 = 0.31;
const MAIN_STRIP_WIDTH: f64 = 0.15;
const SECOND_STRIP_OFFSET: f64 = 0.36;
const SECOND_STRIP_WIDTH: f64 = 0.07;
const SECOND_STRIP_GAIN: f64 = 0.4;
/// 菲涅耳：正面的反射率、掠射时加上的部分与它随角度变化的幂。
const FRESNEL_BASE: f64 = 0.8;
const FRESNEL_GAIN: f64 = 0.2;
const FRESNEL_POWER: i32 = 3;
/// 拉丝的细纹：横纹的频率与深浅，颗粒的两个频率与深浅。
const GRAIN_LINES: f64 = 160.0;
const GRAIN_LINE_DEPTH: f64 = 0.025;
const GRAIN_SPECKLE_X: f64 = 37.0;
const GRAIN_SPECKLE_Y: f64 = 91.0;
const GRAIN_SPECKLE_DEPTH: f64 = 0.012;
/// 侧面：距离场的值在 `STEP × 这个比例` 以内的格画侧面。
const SIDE_BAND: f64 = 0.8;

/// 两套金属与环境的颜色：浅色背景、深色背景。
const METAL_LIGHT: Metal = Metal {
    base: (58, 64, 76),
    sky_top: (150, 158, 172),
    sky_bottom: (70, 76, 88),
    strip: (250, 252, 255),
};
const METAL_DARK: Metal = Metal {
    base: (40, 46, 60),
    sky_top: (150, 162, 186),
    sky_bottom: (38, 44, 58),
    strip: (248, 251, 255),
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Cell {
    pub dots: u8,
    pub rgb: Rgb,
}

/// 姿态：绕竖轴（yaw，正值右边往里）与横轴（pitch，负值下边往里）的角度，整体沿视线的位移（push，负值往里）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pose {
    pub yaw: f64,
    pub pitch: f64,
    pub push: f64,
}

pub const REST: Pose = Pose {
    yaw: 0.0,
    pitch: 0.0,
    push: 0.0,
};

/// 金属与环境的颜色：深色与浅色背景各一套。
pub struct Metal {
    base: Rgb,
    sky_top: Rgb,
    sky_bottom: Rgb,
    strip: Rgb,
}

impl Metal {
    pub fn new(light: bool) -> Self {
        if light { METAL_LIGHT } else { METAL_DARK }
    }

    /// `normal` 与 `view`（指向观察者）都是单位向量；`grain` 是拉丝纹理的明暗（±几个百分点）。
    fn shade(&self, normal: [f64; 3], view: [f64; 3], grain: f64) -> Rgb {
        let facing = normal.iter().zip(view).map(|(n, v)| n * v).sum::<f64>();
        let reflect: [f64; 3] = std::array::from_fn(|i| 2.0 * facing * normal[i] - view[i]);
        // 灯条沿左上—右下的对角线排布：主灯条在左上，副灯条在右下；天光上亮下暗（y 向下）。
        let along = (reflect[0] + reflect[1]) * FRAC_1_SQRT_2;
        let main = (-((along + MAIN_STRIP_OFFSET) / MAIN_STRIP_WIDTH).powi(2)).exp();
        let second = SECOND_STRIP_GAIN
            * (-((along - SECOND_STRIP_OFFSET) / SECOND_STRIP_WIDTH).powi(2)).exp();
        let sky = ((1.0 - reflect[1]) / 2.0).clamp(0.0, 1.0);
        let environment = blend(self.sky_bottom, self.sky_top, sky);
        let environment = blend(environment, self.strip, (main + second).min(1.0));
        // 金属正面就有很强的反射；掠射角越大越强，倒角边缘因此是亮边。
        let fresnel =
            FRESNEL_BASE + FRESNEL_GAIN * (1.0 - facing.clamp(0.0, 1.0)).powi(FRESNEL_POWER);
        let metal = blend(self.base, environment, fresnel);
        let scale = |v: u8| {
            (f64::from(v) * (1.0 + grain))
                .clamp(0.0, f64::from(u8::MAX))
                .round() as u8
        };
        (scale(metal.0), scale(metal.1), scale(metal.2))
    }
}

/// 一帧的点阵：`columns × rows` 个字符，按行排列。
pub fn frame(columns: usize, rows: usize, pose: Pose, metal: &Metal) -> Vec<Cell> {
    let (width, height) = (columns * DOT_COLUMNS, rows * DOT_ROWS);
    let mut depth = vec![f64::NEG_INFINITY; width * height];
    let mut color = vec![(0, 0, 0); width * height];
    let shape = &*FIELD;
    let (yaw_sin, yaw_cos) = pose.yaw.sin_cos();
    let (pitch_sin, pitch_cos) = pose.pitch.sin_cos();
    // 盲文的点在屏幕上接近正方形：按短边缩放，留出透视放大的余量。
    let scale = width.min(height) as f64 / 2.0 / FIT / EXTENT;
    let (center_x, center_y) = (width as f64 / 2.0, height as f64 / 2.0);
    let turn = |[x, y, z]: [f64; 3]| {
        let (x1, z1) = (x * yaw_cos + z * yaw_sin, z * yaw_cos - x * yaw_sin);
        let (y2, z2) = (
            y * pitch_cos - z1 * pitch_sin,
            y * pitch_sin + z1 * pitch_cos,
        );
        [x1, y2, z2]
    };
    let mut plot = |point: [f64; 3], normal: [f64; 3]| {
        let [x, y, z] = turn(point);
        let z = z + pose.push;
        let perspective = VIEW / (VIEW - z);
        let column = (center_x + x * scale * perspective).floor();
        let row = (center_y + y * scale * perspective).floor();
        if column < 0.0 || row < 0.0 || column >= width as f64 || row >= height as f64 {
            return;
        }
        let i = row as usize * width + column as usize;
        if z <= depth[i] {
            return;
        }
        depth[i] = z;
        let normal = turn(normal);
        let to_eye = [-x, -y, VIEW - z];
        let length = to_eye.iter().map(|v| v * v).sum::<f64>().sqrt();
        let view = to_eye.map(|v| v / length);
        // 拉丝：沿横向的细纹，再叠一点不规则的颗粒。
        let grain = (point[1] * GRAIN_LINES).sin() * GRAIN_LINE_DEPTH
            + (point[0] * GRAIN_SPECKLE_X + point[1] * GRAIN_SPECKLE_Y).sin() * GRAIN_SPECKLE_DEPTH;
        color[i] = metal.shade(normal, view, grain);
    };
    for row in 1..GRID - 1 {
        for column in 1..GRID - 1 {
            let i = row * GRID + column;
            let d = f64::from(shape[i]);
            if d < -STEP {
                continue;
            }
            let (x, y) = (column as f64 * STEP - EXTENT, row as f64 * STEP - EXTENT);
            // 距离场向内增大：梯度的反方向是向外的法线。
            let dx = f64::from(shape[i + 1]) - f64::from(shape[i - 1]);
            let dy = f64::from(shape[i + GRID]) - f64::from(shape[i - GRID]);
            let length = dx.hypot(dy).max(f64::EPSILON);
            let (gx, gy) = (dx / length, dy / length);
            if d > 0.0 {
                let edge = (1.0 - d / BEVEL).clamp(0.0, 1.0);
                let nz = (1.0 - edge * edge).sqrt();
                let z = HALF_DEPTH - BEVEL + BEVEL * nz;
                let [nx, ny] = [-gx * edge + x * DOME * nz, -gy * edge + y * DOME * nz];
                let length = (nx * nx + ny * ny + nz * nz).sqrt();
                plot([x, y, z], [nx / length, ny / length, nz / length]);
                plot([x, y, -z], [nx / length, ny / length, -nz / length]);
            }
            if d.abs() < STEP * SIDE_BAND {
                let side = HALF_DEPTH - BEVEL;
                let layers = (side * 2.0 / STEP).ceil() as usize;
                for layer in 0..=layers {
                    let z = -side + layer as f64 / layers as f64 * side * 2.0;
                    plot([x - gx * d, y - gy * d, z], [-gx, -gy, 0.0]);
                }
            }
        }
    }
    let mut cells = vec![Cell::default(); columns * rows];
    for (index, cell) in cells.iter_mut().enumerate() {
        let (row, column) = (index / columns, index % columns);
        let (mut sum, mut count) = ([0u32; 3], 0u32);
        for (dx, bits) in DOT_BITS.iter().enumerate() {
            for (dy, bit) in bits.iter().enumerate() {
                let i = (row * DOT_ROWS + dy) * width + column * DOT_COLUMNS + dx;
                if depth[i].is_finite() {
                    cell.dots |= bit;
                    let (r, g, b) = color[i];
                    sum = [
                        sum[0] + u32::from(r),
                        sum[1] + u32::from(g),
                        sum[2] + u32::from(b),
                    ];
                    count += 1;
                }
            }
        }
        if count > 0 {
            let average = |v: u32| ((v + count / 2) / count) as u8;
            cell.rgb = (average(sum[0]), average(sum[1]), average(sum[2]));
        }
    }
    cells
}

/// 舞台上的字符格 → 徽章上的位置（-1..1，徽章边缘为 ±1）：与 [`frame`] 的投影相同的缩放。
pub fn badge_point(columns: usize, rows: usize, column: f64, row: f64) -> [f64; 2] {
    let (width, height) = ((columns * DOT_COLUMNS) as f64, (rows * DOT_ROWS) as f64);
    let scale = width.min(height) / 2.0 / FIT / EXTENT;
    let half = super::shape::BADGE_HALF;
    [
        ((column * DOT_COLUMNS as f64 - width / 2.0) / scale / half).clamp(-1.0, 1.0),
        ((row * DOT_ROWS as f64 - height / 2.0) / scale / half).clamp(-1.0, 1.0),
    ]
}
