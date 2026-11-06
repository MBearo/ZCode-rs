//! 标志的立体模型（docs/specs/rust-cli/tui.md §7.3「立体模型」）：静止的点放在正面与背面，中间是侧壁。坐标以 Z 的外框中心
//! 为原点，单位是点；x 向右、y 向下、z 朝向观者。数值按设计稿存成 f32。
use super::render::round;
use super::shape::{Dots, HALF};

/// 点的种类：正面、背面、侧壁。
pub(super) const FRONT: u8 = 0;
pub(super) const BACK: u8 = 1;
pub(super) const WALL: u8 = 2;

/// 每个亮点在正面与背面各放四个点，Z 倾向观者时不露缝。
const SUB: [(f64, f64); 4] = [(-0.25, -0.25), (0.25, -0.25), (-0.25, 0.25), (0.25, 0.25)];
/// 侧壁上点的间距。
const WALL_STEP: f64 = 0.5;

/// 厚度：按 Z 的高，至少这么多点。
const THICKNESS: f64 = 0.09;
const MIN_THICKNESS: f64 = 3.0;

pub(super) struct Model {
    pub dots: Dots,
    /// 外框的高与中心（点）。
    pub zh: f64,
    pub cx: f64,
    pub cy: f64,
    /// 厚度：背面在 `z = −T`。
    pub thick: f64,
    pub x: Vec<f32>,
    pub y: Vec<f32>,
    pub z: Vec<f32>,
    pub kind: Vec<u8>,
    pub nx: Vec<f32>,
    pub ny: Vec<f32>,
    pub nz: Vec<f32>,
}

impl Model {
    pub fn new(rows: usize) -> Model {
        let dots = Dots::new(rows);
        let zh = dots.height as f64;
        let (cx, cy) = (dots.zw / 2.0, zh / 2.0);
        let thick = MIN_THICKNESS.max(round(zh * THICKNESS));
        let mut m = Model {
            zh,
            cx,
            cy,
            thick,
            x: Vec::new(),
            y: Vec::new(),
            z: Vec::new(),
            kind: Vec::new(),
            nx: Vec::new(),
            ny: Vec::new(),
            nz: Vec::new(),
            dots,
        };
        let (w, h) = (m.dots.width, m.dots.height);
        // 静止时全亮（Lattice）：每个 Z 的点。
        for y in 0..h {
            for x in 0..w {
                if !m.dots.solid(x as isize, y as isize) {
                    continue;
                }
                let (px, py) = (x as f64 + HALF - cx, y as f64 + HALF - cy);
                for (a, b) in SUB {
                    m.push([px + a, py + b, 0.0], FRONT, [0.0, 0.0, 1.0]);
                    m.push([px + a, py + b, -thick], BACK, [0.0, 0.0, -1.0]);
                }
            }
        }
        // 侧壁：Z 的边界点（上下左右有一个不在 Z 里），按覆盖率的梯度取朝外的法线。
        for y in 0..h as isize {
            for x in 0..w as isize {
                let d = &m.dots;
                if !d.solid(x, y)
                    || (d.solid(x - 1, y)
                        && d.solid(x + 1, y)
                        && d.solid(x, y - 1)
                        && d.solid(x, y + 1))
                {
                    continue;
                }
                let (gx, gy) = (
                    d.cover(x + 1, y) - d.cover(x - 1, y),
                    d.cover(x, y + 1) - d.cover(x, y - 1),
                );
                let len = match gx.hypot(gy) {
                    0.0 => 1.0,
                    len => len,
                };
                let (px, py) = (x as f64 + HALF - cx, y as f64 + HALF - cy);
                let mut z = -WALL_STEP;
                while z > -thick {
                    m.push([px, py, z], WALL, [-gx / len, -gy / len, 0.0]);
                    z -= WALL_STEP;
                }
            }
        }
        m
    }

    fn push(&mut self, at: [f64; 3], kind: u8, normal: [f64; 3]) {
        self.x.push(at[0] as f32);
        self.y.push(at[1] as f32);
        self.z.push(at[2] as f32);
        self.kind.push(kind);
        self.nx.push(normal[0] as f32);
        self.ny.push(normal[1] as f32);
        self.nz.push(normal[2] as f32);
    }

    pub fn len(&self) -> usize {
        self.x.len()
    }

    /// 静止时第 `(x, y)` 个点亮不亮。
    pub fn rest(&self, x: usize, y: usize) -> bool {
        self.dots.solid(x as isize, y as isize)
    }
}
