//! 一帧标志（docs/specs/rust-cli/tui.md §7.3「投影」「画法」）：先在点的分辨率上投影、z 缓冲、打光，得到每个点亮不亮与它的
//! 色调（0 到 1，1 是静止的点）；再把点合成格子，一格一个盲文字符、一个颜色。
use super::model::Model;
use super::press::{Pose, clamp};
use super::shape::{ACROSS, DOWN, HALF};

pub(super) type Rgb = [f64; 3];

pub(super) fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    std::array::from_fn(|i| a[i] + (b[i] - a[i]) * t)
}

/// 与 JS 的 `Math.round` 相同：.5 向上（负数也是）。
pub(super) fn round(v: f64) -> f64 {
    (v + HALF).floor()
}

/// 画布：整块空白处，点的分辨率。`(left, top)` 是 Z 外框左上在画布里的点坐标。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Canvas {
    pub width: usize,
    pub height: usize,
    pub left: isize,
    pub top: isize,
}

pub(super) struct Dots {
    pub lit: Vec<bool>,
    pub tone: Vec<f32>,
}

/// 透视的距离（按 Z 的高）。
const FOCAL: f64 = 3.5;
/// 各种点的底色：正面、背面、侧壁（按 `model` 的种类编号）。
const BASE: [f64; 3] = [1.0, 0.74, 0.8];
/// 光从左上来（未归一）。
const LIGHT: [f64; 3] = [-0.35, -0.45, 0.82];
/// 环境光与漫反射：亮度 = 环境 + 漫反射 × λ。
const AMBIENT: f64 = 0.62;
const DIFFUSE: f64 = 0.38;
/// 雾：远到 `FOG_DEPTH × zh` 时暗这么多。
const FOG: f64 = 0.3;
const FOG_DEPTH: f64 = 0.8;
/// 色阶取整的级数。
const TONE_STEPS: f64 = 12.0;
/// 盲文块的第一个码位。
const BRAILLE: u32 = 0x2800;

fn base(kind: u8) -> f64 {
    BASE[usize::from(kind)]
}

fn light() -> [f64; 3] {
    let v = LIGHT;
    let l = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    v.map(|c| c / l)
}

/// 按 `pose` 摆好的 Z（`None` 是静止的 Z）。
pub(super) fn dots(m: &Model, canvas: Canvas, pose: Option<Pose>) -> Dots {
    let n = canvas.width * canvas.height;
    let mut lit = vec![false; n];
    let mut tone = vec![0f32; n];
    let Some(pose) = pose else {
        for y in 0..m.dots.height {
            for x in 0..m.dots.width {
                if let Some(j) = canvas.index(x as isize, y as isize)
                    && m.rest(x, y)
                {
                    lit[j] = true;
                    tone[j] = 1.0;
                }
            }
        }
        return Dots { lit, tone };
    };
    let mut depth = vec![f32::NEG_INFINITY; n];
    let placing = pose.on(m);
    let focal = FOCAL * m.zh;
    // 透视在一串按压的开头与结尾渐入渐出（`Pose::perspective`）：静止时背面与侧壁正好藏在正面后面，第一帧与最后一帧就与
    // 静止的 Z 相同。
    let fade_in = pose.perspective;
    let l = light();
    let lambert_rest = AMBIENT + DIFFUSE * l[2];
    for i in 0..m.len() {
        let p = placing.place(i);
        // 设计稿把每个点的样子存在 f32 里：照做，取整的边界才一致。
        let [x, y, z] = p.at.map(|v| f64::from(v as f32));
        let k = 1.0 + (focal / (focal - z) - 1.0) * fade_in;
        if k <= 0.0 {
            continue;
        }
        let (sx, sy) = ((m.cx + x * k).floor(), (m.cy + y * k).floor());
        let Some(j) = canvas.index(sx as isize, sy as isize) else {
            continue;
        };
        if z <= f64::from(depth[j]) {
            continue;
        }
        depth[j] = z as f32;
        let [nx, ny, nz] = p.normal.map(|v| f64::from(v as f32));
        let len = match (nx * nx + ny * ny + nz * nz).sqrt() {
            0.0 => 1.0,
            len => len,
        };
        let lambert = ((nx * l[0] + ny * l[1] + nz * l[2]) / len).max(0.0);
        let fog = 1.0 - FOG * clamp(-z / (m.zh * FOG_DEPTH));
        lit[j] = true;
        tone[j] = ((base(m.kind[i]) * (AMBIENT + DIFFUSE * lambert) / lambert_rest).min(1.0) * fog)
            as f32;
    }
    Dots { lit, tone }
}

impl Canvas {
    /// Z 外框里的点 `(x, y)` 在画布里的下标；出了画布是 `None`。
    fn index(&self, x: isize, y: isize) -> Option<usize> {
        let (x, y) = (x + self.left, y + self.top);
        (x >= 0 && y >= 0 && (x as usize) < self.width && (y as usize) < self.height)
            .then(|| y as usize * self.width + x as usize)
    }
}

/// 点位：左列自上而下 `0x01 0x02 0x04 0x40`，右列 `0x08 0x10 0x20 0x80`。
const BIT: [[u32; 2]; 4] = [[0x01, 0x08], [0x02, 0x10], [0x04, 0x20], [0x40, 0x80]];

/// 一格：盲文字符与颜色（已取整）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Cell {
    pub glyph: char,
    pub rgb: [u8; 3],
}

/// 墨：静止色与点亮后的颜色（§7.3「墨」）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Inks {
    pub bg: Rgb,
    pub rest: Rgb,
    pub peak: Rgb,
}

/// 把点合成格子：`cols × rows` 格，行优先；没有亮点的格子是 `None`。`sal` 是显著度。
pub(super) fn cells(frame: &Dots, canvas: Canvas, inks: &Inks, sal: f64) -> Vec<Option<Cell>> {
    let ink = mix(inks.rest, inks.peak, sal);
    let (cols, rows) = (canvas.width / ACROSS, canvas.height / DOWN);
    let mut out = vec![None; cols * rows];
    for r in 0..rows {
        for c in 0..cols {
            let (mut bits, mut sum, mut n) = (0u32, 0f64, 0u32);
            for (dy, row) in BIT.iter().enumerate() {
                for (dx, bit) in row.iter().enumerate() {
                    let j = (r * DOWN + dy) * canvas.width + c * ACROSS + dx;
                    if frame.lit[j] {
                        bits |= bit;
                        sum += f64::from(frame.tone[j]);
                        n += 1;
                    }
                }
            }
            if bits == 0 {
                continue;
            }
            // 色阶取整到 1/12：相邻的格更容易同色，渲染器的样式段就更长。
            let q = round(sum / f64::from(n) * TONE_STEPS) / TONE_STEPS;
            let col = mix(inks.bg, ink, q);
            out[r * cols + c] = Some(Cell {
                glyph: char::from_u32(BRAILLE + bits).unwrap_or(' '),
                rgb: col.map(|v| round(v).clamp(0.0, f64::from(u8::MAX)) as u8),
            });
        }
    }
    out
}
