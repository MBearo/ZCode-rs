//! 标志的形状（docs/specs/rust-cli/tui.md §7.3）：品牌 Z 的三条路径（`ZCodeAboutLogo.tsx`，viewBox 256 × 218）在盲文点格上的
//! 覆盖率。每个点 3 × 3 采样。
const VIEW_WIDTH: f64 = 256.0;
const VIEW_HEIGHT: f64 = 218.0;
/// 每个点每边的采样数。
const SAMPLES: usize = 3;
/// 一段三次贝塞尔展开成的折线段数。
const CURVE_STEPS: usize = 12;
/// 三次伯恩斯坦多项式中间两项的系数。
const BERNSTEIN: f64 = 3.0;
/// 一格的点：横 2 个、竖 4 个（盲文）。
pub(super) const ACROSS: usize = 2;
pub(super) const DOWN: usize = 4;
/// 一半：点与格子的中心、覆盖率过半才算 Z 的点。
pub(super) const HALF: f64 = 0.5;

enum Seg {
    Line(f64, f64),
    Curve([f64; 6]),
}
use Seg::{Curve, Line};

/// 上横、斜杠、下横：每条是起点与闭合的路径。
const PATHS: [((f64, f64), &[Seg]); 3] = [
    (
        (134.4, 0.130152),
        &[
            Line(116.48, 25.6022),
            Curve([113.665, 29.5699, 109.054, 32.0019, 104.064, 32.0019]),
            Line(6.3999, 32.0019),
            Line(6.3999, 0.0),
            Curve([6.3999, 0.130149, 134.4, 0.130152, 134.4, 0.130152]),
        ],
    ),
    (
        (256.0, 0.130127),
        &[
            Line(102.401, 217.732),
            Line(0.0, 217.732),
            Line(153.599, 0.130127),
            Line(256.0, 0.130127),
        ],
    ),
    (
        (121.601, 217.732),
        &[
            Line(139.65, 192.134),
            Curve([142.465, 188.166, 147.076, 185.734, 152.067, 185.734]),
            Line(249.604, 185.734),
            Line(249.604, 217.736),
            Line(121.601, 217.736),
            Line(121.601, 217.732),
        ],
    ),
];

fn polygon(start: (f64, f64), segs: &[Seg]) -> Vec<(f64, f64)> {
    let mut points = vec![start];
    let mut at = start;
    for seg in segs {
        match *seg {
            Line(x, y) => at = (x, y),
            Curve([ax, ay, bx, by, x, y]) => {
                for step in 1..CURVE_STEPS {
                    let t = step as f64 / CURVE_STEPS as f64;
                    let u = 1.0 - t;
                    let k = [
                        u * u * u,
                        BERNSTEIN * u * u * t,
                        BERNSTEIN * u * t * t,
                        t * t * t,
                    ];
                    points.push((
                        k[0] * at.0 + k[1] * ax + k[2] * bx + k[3] * x,
                        k[0] * at.1 + k[1] * ay + k[2] * by + k[3] * y,
                    ));
                }
                at = (x, y);
            }
        }
        points.push(at);
    }
    points
}

/// 点 `(x, y)`（viewBox 坐标）在不在 Z 里：每条路径各按奇偶规则。
fn inside(polygons: &[Vec<(f64, f64)>], x: f64, y: f64) -> bool {
    polygons.iter().any(|points| {
        let mut hit = false;
        let mut last = points[points.len() - 1];
        for &point in points {
            if (point.1 > y) != (last.1 > y)
                && x < point.0 + (y - point.1) / (last.1 - point.1) * (last.0 - point.0)
            {
                hit = !hit;
            }
            last = point;
        }
        hit
    })
}

/// Z 宽多少（不取整）：高 `height` 时，单位随意。
pub(super) fn width_of(height: usize) -> f64 {
    height as f64 * VIEW_WIDTH / VIEW_HEIGHT
}

/// `rows` 行高的 Z 在盲文点格上的覆盖率（一格 2 × 4 个点，点是方的）。坐标以 Z 的外框左上为原点。
pub(super) struct Dots {
    pub rows: usize,
    /// 点格的宽与高（宽是 `zw` 向上取整）。
    pub width: usize,
    pub height: usize,
    /// 不取整的宽：中心按它算。
    pub zw: f64,
    cover: Vec<f32>,
}

impl Dots {
    pub fn new(rows: usize) -> Dots {
        let height = rows * DOWN;
        let zw = width_of(height);
        let width = zw.ceil() as usize;
        let unit = VIEW_HEIGHT / height as f64;
        let polygons: Vec<_> = PATHS.iter().map(|(s, segs)| polygon(*s, segs)).collect();
        let samples = (SAMPLES * SAMPLES) as f32;
        let mut cover = Vec::with_capacity(width * height);
        for y in 0..height {
            for x in 0..width {
                let mut hits = 0u32;
                for a in 0..SAMPLES {
                    for b in 0..SAMPLES {
                        let sx = (x as f64 + (a as f64 + HALF) / SAMPLES as f64) * unit;
                        let sy = (y as f64 + (b as f64 + HALF) / SAMPLES as f64) * unit;
                        hits += u32::from(inside(&polygons, sx, sy));
                    }
                }
                cover.push(hits as f32 / samples);
            }
        }
        Dots {
            rows,
            width,
            height,
            zw,
            cover,
        }
    }

    /// 第 `(x, y)` 个点的覆盖率；点格之外是 0。
    pub fn cover(&self, x: isize, y: isize) -> f64 {
        if x < 0 || y < 0 || x as usize >= self.width || y as usize >= self.height {
            return 0.0;
        }
        f64::from(self.cover[y as usize * self.width + x as usize])
    }

    /// 覆盖率至少一半的点是 Z 的点。
    pub fn solid(&self, x: isize, y: isize) -> bool {
        self.cover(x, y) >= HALF
    }
}
