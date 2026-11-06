//! 生命迹象（docs/puffin-tui.md §4.2「笔」的「手停住」）：agent 的手停住半秒之后屏幕上的东西，不论它在等什么。三列点，光在其中
//! 滚过，再接着走过后面的那个词（[`band`]）；在等模型的地方，后面跟着 `Working` 与回合的时钟：`⋮⋮⋮ Working  7s`。
//! 移植自 packages/tui/tui/src/zcode/tui/shell/Lattice.scala（feat/tui a2bf878b34）第 1–41 行。
//! 从 Puffin 的壳（shell/lattice.rs）搬进 kit（docs/specs/rust-cli/tui-kit.md §4.4），两套界面都调；行为不变。
use crate::app::widgets::fade;
use crate::render::{Color, Span, Style, attr};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

pub const DOTS: &str = "⋮";
/// 点出现之前手停住的 tick 数：词之间的停顿从不让它们闪现。
pub const GRACE_TICKS: i64 = 15;
/// 光滚过三列一次（约 1.15 s）。
pub const PERIOD_TICKS: i64 = 35;
/// 每列落后前一列多少弧度：约六分之一圈。
pub const LAG: f64 = 1.05;
/// plain 模式：这么亮的列加粗（三列都是 brand）。
pub const PLAIN_BRIGHT: f64 = 0.66;
/// 每一列的亮色对地的对比度：色相只改亮度，直到达到它。
pub const PEAK: f64 = 7.0;
/// 光带在词上每个字前进多少列。
const BAND_STEP: f64 = 0.55;
/// 光带的窄：亮度取这么多次方。
const BAND_POWER: i32 = 6;

/// 出现 `age` 个 tick 之后的点，从背景升起。
pub fn sign(theme: &Theme, age: i64) -> Vec<Span> {
    fade::fade_in(theme, dots(theme, age), age, fade::TICKS, None)
}

/// 出现 `age` 个 tick 之后的工作行，回合 `turn_ms` 长。
pub fn line(theme: &Theme, age: i64, turn_ms: f64) -> Vec<Span> {
    let mut spans = dots(theme, age);
    spans.push(Span::new(" ", Style::DEFAULT));
    spans.extend(band(theme, age, Span::new("Working", theme.fg2)));
    if turn_ms >= 1000.0 {
        spans.push(Span::new("  ", Style::DEFAULT));
        spans.push(Span::new(clock(turn_ms), theme.fg3));
    }
    fade::fade_in(theme, spans, age, fade::TICKS, None)
}

/// 第 `col` 列在 `age` 个 tick 时的亮度：峰值 1，最低 0。
pub fn bright(col: usize, age: i64) -> f64 {
    let phase = age.rem_euclid(PERIOD_TICKS) as f64;
    0.5 + 0.5 * (2.0 * std::f64::consts::PI * phase / PERIOD_TICKS as f64 - LAG * col as f64).cos()
}

/// 光带在词的第 `i` 个字上的混合量：点阵的亮度接着往后走（列号 `3 + 0.55·i`），取 6 次方，所以光带窄、走到词尾就淡没。
pub fn band_k(i: usize, age: i64) -> f64 {
    let phase = age.rem_euclid(PERIOD_TICKS) as f64;
    let col = 3.0 + BAND_STEP * i as f64;
    let k =
        0.5 + 0.5 * (2.0 * std::f64::consts::PI * phase / PERIOD_TICKS as f64 - LAG * col).cos();
    k.powi(BAND_POWER)
}

/// 点阵后面隔一格的那个词 `word`（`Working`、调用的名称、`N running`），点阵出现 `age` 个 tick：亮光离开第三列之后接着走过
/// 它，每个字从它自己此刻的颜色混向第三列的亮色；字形与字重不变。plain 模式下光带上的字去掉 dim。
pub fn band(theme: &Theme, age: i64, word: Span) -> Vec<Span> {
    let style = word.style;
    let to = lit(theme, 2);
    word.text
        .chars()
        .enumerate()
        .map(|(i, ch)| {
            let k = band_k(i, age);
            let st = match (to, style.fg_color()) {
                (Some(to), Color::TrueColor(r, g, b)) => {
                    style.with_fg(Rgb::new(r, g, b).mix(to, k).color())
                }
                (None, _) if k > 0.5 => Style::new(
                    style.fg_color(),
                    style.bg_color(),
                    style.attrs() & !attr::DIM,
                ),
                _ => style,
            };
            Span::new(ch.to_string(), st)
        })
        .collect()
}

/// 第 `col` 列的亮色：它的色相（brand、brand 与 work 的中点、work）只改 OKLCH 亮度，直到对地达到 [`PEAK`]。plain 模式没有。
fn lit(theme: &Theme, col: usize) -> Option<Rgb> {
    match (theme.bg, theme.brand, theme.work) {
        (Some(ground), Color::TrueColor(br, bg, bb), Color::TrueColor(wr, wg, wb))
            if theme.rich =>
        {
            let hue = Rgb::new(br, bg, bb).mix(Rgb::new(wr, wg, wb), col as f64 / 2.0);
            Some(Theme::lift(hue, &[ground], PEAK, ground.lightness() < 0.5))
        }
        _ => None,
    }
}

fn dots(theme: &Theme, age: i64) -> Vec<Span> {
    (0..3)
        .map(|col| {
            let k = bright(col, age);
            match (theme.fg2.fg_color(), lit(theme, col)) {
                // 修复原因：以前点在 line 色与色相之间变化（浅色的地上色相再向正文色混 35%）。line 色对地只有 1.4–1.7:1，`⋮`
                // 又只有几个像素的墨：一个周期里一半时间低于 3:1，最亮时也只比工具名亮一点，比旁边的句子淡。现在最暗是 fg2，
                // 最亮是对地 7:1、色相不变的亮色，粗体，深浅两种地相同（docs/puffin-tui.md §4.2「手停住」）。
                (Color::TrueColor(r, g, b), Some(to)) => Span::new(
                    DOTS,
                    Style::fg(Rgb::new(r, g, b).mix(to, k).color()).with_attr(attr::BOLD),
                ),
                // plain：三点都是蓝，最亮的那一列加粗。
                _ if k > PLAIN_BRIGHT => {
                    Span::new(DOTS, Style::fg(theme.brand).with_attr(attr::BOLD))
                }
                _ => Span::new(DOTS, Style::fg(theme.brand)),
            }
        })
        .collect()
}

/// 走着的墙钟：整秒，过了一分钟 `1m 05s`；没有十分之一秒，它们会闪。从壳的 `subagents.rs` 搬来（工作的行要它；壳的
/// `subagents::clock` 再导出它）。
pub fn clock(ms: f64) -> String {
    let secs = ((ms / 1000.0) as i64).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else {
        format!("{}m {:02}s", secs / 60, secs % 60)
    }
}
