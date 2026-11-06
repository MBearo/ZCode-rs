//! 终端的颜色。移植自 packages/tui/tui/src/zcode/tui/render/Style.scala 的 `Color`（feat/tui a2bf878b34）。

/// 一个前景或背景色。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Color {
    /// 终端自己的默认色。
    #[default]
    Default,
    /// 标准（0..7）或亮（8..15）调色板。
    Named(u8),
    /// 256 色立方体的下标。
    Indexed(u8),
    TrueColor(u8, u8, u8),
}

impl Color {
    pub const BLACK: Color = Color::Named(0);
    pub const RED: Color = Color::Named(1);
    pub const GREEN: Color = Color::Named(2);
    pub const YELLOW: Color = Color::Named(3);
    pub const BLUE: Color = Color::Named(4);
    pub const MAGENTA: Color = Color::Named(5);
    pub const CYAN: Color = Color::Named(6);
    pub const WHITE: Color = Color::Named(7);

    /// 24 位真彩。
    pub const fn rgb(r: u8, g: u8, b: u8) -> Color {
        Color::TrueColor(r, g, b)
    }

    pub const fn is_default(self) -> bool {
        matches!(self, Color::Default)
    }

    /// 这个颜色作为前景（`is_fg`）或背景接在 SGR 参数后面的部分，带前导的 `;`；默认色不写。
    pub(crate) fn append_sgr(self, out: &mut String, is_fg: bool) {
        use std::fmt::Write as _;
        match self {
            Color::Default => {}
            Color::Named(i) => {
                let base = if i < 8 {
                    (if is_fg { 30 } else { 40 }) + u32::from(i)
                } else {
                    (if is_fg { 90 } else { 100 }) + u32::from(i - 8)
                };
                let _ = write!(out, ";{base}");
            }
            Color::Indexed(n) => {
                let _ = write!(out, "{};{n}", if is_fg { ";38;5" } else { ";48;5" });
            }
            Color::TrueColor(r, g, b) => {
                let _ = write!(out, "{};{r};{g};{b}", if is_fg { ";38;2" } else { ";48;2" });
            }
        }
    }
}

/// 终端能显示的颜色数（docs/specs/rust-cli/tui-kit.md §3.4）。主题只写真彩；渲染器的样式池按深度把真彩降成终端认得的
/// 颜色（[`ColorDepth::reduce`]），界面代码不用管终端的级别。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum ColorDepth {
    /// 24 位真彩：原样输出。
    #[default]
    TrueColor,
    /// xterm 的 256 色：真彩降成最近的色块或灰阶格。
    Ansi256,
    /// 16 色：真彩与 256 色的格子都丢掉（终端的默认色），只留调色板的 16 色。
    Ansi16,
}

impl ColorDepth {
    /// 环境变量给出的深度（旧界面 `ColorLevel::detect` 的规则，不含 Windows 的版本判断）：`COLORTERM` 是 `truecolor` /
    /// `24bit`（不分大小写）为真彩；`TERM` 含 `256` 或 `direct` 为 256 色；否则 16 色。
    pub fn detect(vars: &std::collections::BTreeMap<String, String>) -> ColorDepth {
        let colorterm = vars.get("COLORTERM").map(|v| v.to_ascii_lowercase());
        if colorterm.is_some_and(|v| v == "truecolor" || v == "24bit") {
            return ColorDepth::TrueColor;
        }
        let term = vars.get("TERM").map(String::as_str).unwrap_or_default();
        if term.contains("256") || term.contains("direct") {
            return ColorDepth::Ansi256;
        }
        ColorDepth::Ansi16
    }

    /// 至少 256 色：`Theme::resolve` 的 `colorful`（富主题降采样之后仍可读；16 色是 plain）。
    pub const fn colorful(self) -> bool {
        !matches!(self, ColorDepth::Ansi16)
    }

    /// `c` 在这个深度上的样子：真彩时原样；256 色时 `TrueColor` 换成 [`nearest_256`] 的格子；16 色时 `TrueColor` 与
    /// 16 以上的 `Indexed` 都换成 `Default`。`Default` 与 `Named` 总是原样。
    pub fn reduce(self, c: Color) -> Color {
        match (self, c) {
            (ColorDepth::TrueColor, _) | (_, Color::Default | Color::Named(_)) => c,
            (ColorDepth::Ansi256, Color::TrueColor(r, g, b)) => {
                Color::Indexed(nearest_256(r, g, b))
            }
            (ColorDepth::Ansi256, Color::Indexed(_)) => c,
            (ColorDepth::Ansi16, Color::Indexed(n)) if n < 16 => c,
            (ColorDepth::Ansi16, _) => Color::Default,
        }
    }
}

/// 256 色立方体（6×6×6）每一档的分量。
const CUBE_STEPS: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// xterm 256 色里与 (`r`, `g`, `b`) 最近的一格（6×6×6 色块与 24 级灰阶）。
/// 移植自旧界面 crates/tui/src/ui/palette.rs 的 `nearest_256`，行为相同，包括修复线 F26 修过的蓝分量缺陷（以前距离函数的
/// 参数遮住了目标的蓝分量，偏蓝的颜色落到灰阶上）。
pub fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    let index = |v: u8| {
        CUBE_STEPS
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| (i32::from(**s) - i32::from(v)).abs())
            .map_or(0, |(i, _)| i as u8)
    };
    let (ri, gi, bi) = (index(r), index(g), index(b));
    let cube = 16 + 36 * ri + 6 * gi + bi;
    let cube_rgb = (
        CUBE_STEPS[ri as usize],
        CUBE_STEPS[gi as usize],
        CUBE_STEPS[bi as usize],
    );
    let gray_level = ((u32::from(r) + u32::from(g) + u32::from(b)) / 3).min(238);
    let gray_index = (gray_level.saturating_sub(8) / 10).min(23) as u8;
    let gray = 8 + 10 * gray_index;
    let distance = |(cr, cg, cb): (u8, u8, u8)| {
        let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2);
        d(cr, r) + d(cg, g) + d(cb, b)
    };
    if distance((gray, gray, gray)) < distance(cube_rgb) {
        232 + gray_index
    } else {
        cube
    }
}

/// 256 色里色块与灰阶格（16..=255）的标准 RGB（xterm 的取值）；前 16 色由终端的调色板决定，`None`。
/// 渲染器降采样涂地的颜色时用它：OSC 10/11 只收 RGB，写成格子的 RGB，终端的边距与格子的地一样（§3.4）。
pub fn indexed_rgb(n: u8) -> Option<(u8, u8, u8)> {
    match n {
        0..=15 => None,
        16..=231 => {
            let i = n - 16;
            Some((
                CUBE_STEPS[usize::from(i / 36)],
                CUBE_STEPS[usize::from(i / 6 % 6)],
                CUBE_STEPS[usize::from(i % 6)],
            ))
        }
        232..=255 => {
            let v = 8 + 10 * (n - 232);
            Some((v, v, v))
        }
    }
}
