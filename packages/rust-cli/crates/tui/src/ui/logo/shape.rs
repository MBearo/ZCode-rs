// 复制自 apps/zcode-host/crates/tui/src/ui/logo/shape.rs（2f982570bb）；只改了 spec 的出处。
//! 徽章的形状（docs/specs/rust-cli/tui.md §7.3；原为 rust-tui.md §8.11）：Z 标志的矢量路径 → 二维有符号距离场（内部为正）。
//! 圆角方块里 Z 是镂空的（应用图标）。进程里只算一次。
use std::sync::LazyLock;

/// 距离场的边长（采样点数）与覆盖范围 [-EXTENT, EXTENT]。
pub(super) const GRID: usize = 128;
pub(super) const EXTENT: f64 = 1.0;
pub(super) const STEP: f64 = EXTENT * 2.0 / (GRID - 1) as f64;

/// `ZCodeAboutLogo.tsx` 的 Z 标志：上横、斜杠、下横，viewBox 0 0 256 218。
const Z_PATHS: [&str; 3] = [
    "M134.4 0.130152L116.48 25.6022C113.665 29.5699 109.054 32.0019 104.064 32.0019H6.3999V0C6.3999 0.130149 134.4 0.130152 134.4 0.130152Z",
    "M256 0.130127L102.401 217.732H0L153.599 0.130127H256Z",
    "M121.601 217.732L139.65 192.134C142.465 188.166 147.076 185.734 152.067 185.734H249.604V217.736H121.601V217.732Z",
];
const Z_SIZE: (f64, f64) = (256.0, 218.0);
/// 徽章的半边长与圆角半径；Z 在徽章里约占边长的 62%。
pub(super) const BADGE_HALF: f64 = 0.86;
const BADGE_RADIUS: f64 = 0.24;
const BADGE_Z_WIDTH: f64 = BADGE_HALF * 2.0 * 0.62;

/// Z 镂空的徽章。
pub(super) static FIELD: LazyLock<Vec<f32>> = LazyLock::new(|| {
    let z = outline(BADGE_Z_WIDTH);
    field(|p| round_square(p).min(-signed(&z, p)))
});

type Point = [f64; 2];

/// 路径的数与命令（只有绝对坐标的 M/L/H/V/C/Z，数都不为负）。
fn tokens(path: &str) -> Vec<Result<char, f64>> {
    let mut out = Vec::new();
    let mut number = String::new();
    let flush = |number: &mut String, out: &mut Vec<Result<char, f64>>| {
        if let Ok(value) = number.parse() {
            out.push(Err(value));
        }
        number.clear();
    };
    for c in path.chars() {
        if c.is_ascii_digit() || (c == '.' && !number.contains('.')) {
            number.push(c);
            continue;
        }
        flush(&mut number, &mut out);
        if c == '.' {
            number.push(c);
        } else if c.is_ascii_alphabetic() {
            out.push(Ok(c));
        }
    }
    flush(&mut number, &mut out);
    out
}

/// 一条路径展开成折线（三次贝塞尔分 16 段），坐标是 viewBox 里的。
fn polyline(path: &str) -> Vec<Point> {
    let tokens = tokens(path);
    let mut numbers = tokens.iter().filter_map(|t| t.err());
    let mut points: Vec<Point> = Vec::new();
    let mut at = [0.0, 0.0];
    for command in tokens.iter().filter_map(|t| t.ok()) {
        let mut next = || numbers.next().unwrap_or(0.0);
        match command {
            'M' | 'L' => at = [next(), next()],
            'H' => at[0] = next(),
            'V' => at[1] = next(),
            'C' => {
                let (a, b, end) = ([next(), next()], [next(), next()], [next(), next()]);
                let from = at;
                for step in 1..16 {
                    let t = f64::from(step) / 16.0;
                    let u = 1.0 - t;
                    let k = [u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t];
                    points.push(std::array::from_fn(|i| {
                        k[0] * from[i] + k[1] * a[i] + k[2] * b[i] + k[3] * end[i]
                    }));
                }
                at = end;
            }
            _ => continue,
        }
        points.push(at);
    }
    points
}

/// Z 的三条闭合折线，缩放到宽度 `width`、居中在原点（y 向下，与屏幕的行方向相同）。
fn outline(width: f64) -> Vec<Vec<Point>> {
    let scale = width / Z_SIZE.0;
    Z_PATHS
        .iter()
        .map(|path| {
            polyline(path)
                .into_iter()
                .map(|[x, y]| [(x - Z_SIZE.0 / 2.0) * scale, (y - Z_SIZE.1 / 2.0) * scale])
                .collect()
        })
        .collect()
}

fn segment_distance(p: Point, a: Point, b: Point) -> f64 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let length = dx * dx + dy * dy;
    let t = if length == 0.0 {
        0.0
    } else {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / length).clamp(0.0, 1.0)
    };
    (p[0] - a[0] - t * dx).hypot(p[1] - a[1] - t * dy)
}

/// 到折线的有符号距离：内部为正（三条路径互不相交，用奇偶规则判断内外）。
fn signed(shapes: &[Vec<Point>], p: Point) -> f64 {
    let mut distance = f64::INFINITY;
    let mut inside = false;
    for shape in shapes {
        for (i, &a) in shape.iter().enumerate() {
            let b = shape[(i + 1) % shape.len()];
            distance = distance.min(segment_distance(p, a, b));
            if (a[1] > p[1]) != (b[1] > p[1])
                && p[0] < a[0] + (p[1] - a[1]) / (b[1] - a[1]) * (b[0] - a[0])
            {
                inside = !inside;
            }
        }
    }
    if inside { distance } else { -distance }
}

/// 圆角方块（内部为正）。
fn round_square([x, y]: Point) -> f64 {
    let inner = BADGE_HALF - BADGE_RADIUS;
    let (qx, qy) = (x.abs() - inner, y.abs() - inner);
    let outside = qx.max(0.0).hypot(qy.max(0.0)) + qx.max(qy).min(0.0);
    BADGE_RADIUS - outside
}

fn field(distance: impl Fn(Point) -> f64) -> Vec<f32> {
    (0..GRID * GRID)
        .map(|i| {
            let (row, column) = (i / GRID, i % GRID);
            let p = [column as f64 * STEP - EXTENT, row as f64 * STEP - EXTENT];
            distance(p) as f32
        })
        .collect()
}

/// 距离场在网格坐标上的值（测试用）。
#[cfg(test)]
pub(super) fn sample(field: &[f32], [x, y]: Point) -> f32 {
    let column = ((x + EXTENT) / STEP).round() as usize;
    let row = ((y + EXTENT) / STEP).round() as usize;
    field[row.min(GRID - 1) * GRID + column.min(GRID - 1)]
}
