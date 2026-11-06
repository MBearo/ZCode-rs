//! 点击时的按压（docs/specs/rust-cli/tui.md §7.3「点击」）：按下的一侧下沉、对侧翘起，再弹回。正在动时再点，弹簧相加，
//! 碰到挡块为止。长度按 Z 的高 `zh`（点）量，时刻都是秒；一次按压的数值是设计稿（2026-11-03）的 Press。
use std::f64::consts::TAU;

use super::model::Model;
use super::shape::HALF;

/// 一次按压的时长。
pub const SECS: f64 = 1.3;
/// 弹簧：振幅、衰减时间、每秒圈数；`SETTLE` 秒起用 `SETTLE_SPAN` 秒归零。
const SPRING: f64 = 1.6;
const DECAY: f64 = 0.32;
const HERTZ: f64 = 1.5;
const SETTLE: f64 = 0.85;
const SETTLE_SPAN: f64 = 0.45;
/// 倾斜（弧度）与下沉（按 `zh`）的幅度；按在边上只沉一半。
const TILT: f64 = 0.62;
const SINK: f64 = 0.16;
/// 挡块：相加的弹簧最多到这么多（一次按压最大约 1.0，到不了它）。
const STOP: f64 = 1.25;
/// 透视在一串按压的开头与结尾渐入渐出的时长。
const PERSPECTIVE_FADE: f64 = 0.15;

pub fn clamp(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// smootherstep：6t⁵ − 15t⁴ + 10t³。
pub fn smooth(v: f64) -> f64 {
    const SMOOTHER: [f64; 3] = [6.0, 15.0, 10.0];
    let t = clamp(v);
    t * t * t * (t * (t * SMOOTHER[0] - SMOOTHER[1]) + SMOOTHER[2])
}

/// 弹簧：按下 `a` 秒后，衰减的正弦乘上收尾。
fn spring(a: f64) -> f64 {
    if a <= 0.0 || a >= SECS {
        return 0.0;
    }
    SPRING
        * (-a / DECAY).exp()
        * (TAU * HERTZ * a).sin()
        * (1.0 - smooth((a - SETTLE) / SETTLE_SPAN))
}

/// 一次按压：从第几秒起（相对 `Mark::epoch`）、点击点（相对 Z 外框左上的点坐标）。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Press {
    pub start: f64,
    pub point: [f64; 2],
}

impl Press {
    pub fn end(&self) -> f64 {
        self.start + SECS
    }
}

/// 3 × 3 的矩阵，按行。
type Mat = [[f64; 3]; 3];

fn rx(a: f64) -> Mat {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

fn ry(a: f64) -> Mat {
    let (s, c) = a.sin_cos();
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}

fn mm(a: Mat, b: Mat) -> Mat {
    std::array::from_fn(|r| {
        std::array::from_fn(|c| a[r][0] * b[0][c] + a[r][1] * b[1][c] + a[r][2] * b[2][c])
    })
}

fn apply(m: Mat, v: [f64; 3]) -> [f64; 3] {
    std::array::from_fn(|r| m[r][0] * v[0] + m[r][1] * v[1] + m[r][2] * v[2])
}

/// 一个点在这一帧的位置与法线。
#[derive(Clone, Copy)]
pub struct Placed {
    pub at: [f64; 3],
    pub normal: [f64; 3],
}

/// 这一帧整块 Z 的姿态：绕竖轴、横轴转多少，沉多少（点），透视用了几成。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pose {
    pub turn: [f64; 2],
    pub sink: f64,
    pub perspective: f64,
}

/// `at` 秒时的姿态：`span` 是这一串相连的按压的开始与结束；不在其中（静止）时 `None`。
pub fn pose(m: &Model, presses: &[Press], span: (f64, f64), at: f64) -> Option<Pose> {
    let (first, last) = span;
    if at <= first || at >= last {
        return None;
    }
    let (zh, zw) = (m.zh, m.dots.zw);
    let (mut across, mut down, mut sink) = (0.0, 0.0, 0.0);
    // 每一项照设计稿的乘法次序算：一次按压时与它的 Press 一位不差。
    for press in presses {
        let d = spring(at - press.start);
        let px = ((press.point[0] - m.cx) / (zw / 2.0)).clamp(-1.0, 1.0);
        let py = ((press.point[1] - m.cy) / (zh / 2.0)).clamp(-1.0, 1.0);
        across += TILT * px * d;
        down += -TILT * py * d;
        sink += -SINK * zh * d * (1.0 - HALF * px.hypot(py).min(1.0));
    }
    let stop = |v: f64, unit: f64| v.clamp(-STOP * unit, STOP * unit);
    Some(Pose {
        turn: [stop(across, TILT), stop(down, TILT)],
        sink: stop(sink, SINK * zh),
        perspective: smooth((at - first).min(last - at) / PERSPECTIVE_FADE),
    })
}

/// 姿态落到每个点上：绕厚度中间转，再沿 z 平移。
pub struct Placing<'m> {
    m: &'m Model,
    turn: Mat,
    pivot: f64,
    sink: f64,
}

impl Pose {
    pub fn on(self, m: &Model) -> Placing<'_> {
        Placing {
            m,
            turn: mm(ry(self.turn[0]), rx(self.turn[1])),
            pivot: -m.thick / 2.0,
            sink: self.sink,
        }
    }
}

impl Placing<'_> {
    /// 第 `i` 个点的样子；`f32` 转回 `f64`。
    pub fn place(&self, i: usize) -> Placed {
        let m = self.m;
        let p = [
            f64::from(m.x[i]),
            f64::from(m.y[i]),
            f64::from(m.z[i]) - self.pivot,
        ];
        let v = apply(self.turn, p);
        Placed {
            at: [v[0], v[1], v[2] + self.pivot + self.sink],
            normal: apply(
                self.turn,
                [f64::from(m.nx[i]), f64::from(m.ny[i]), f64::from(m.nz[i])],
            ),
        }
    }
}
