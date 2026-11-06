//! 壳除了光标的呼吸与流式文字之外唯一的动效（docs/puffin-tui.md §4.2「动效的原则」）：新的东西从它画在其上的底色升到
//! 自己的墨色，作为一整块，几个 tick 完成。
//! 移植自 packages/tui/tui/src/zcode/tui/shell/Fade.scala（feat/tui a2bf878b34）第 1–48 行（全部；B 组可以直接用）。
//! 从 Puffin 的壳（shell/fade.rs）搬进 kit（docs/specs/rust-cli/tui-kit.md §4.4），两套界面都调；行为不变。
use crate::render::{Color, Span, Style};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

/// 一次淡入的 tick 数：工具调用的行与结果、diff、通知、标签、值、面板。
pub const TICKS: i64 = 4;
/// 落下的输出行与跑马灯的新词的 tick 数。
pub const LAND_TICKS: i64 = 3;

/// `spans` 出现 `age` 个 tick 之后、淡入 `ticks` 长时的样子：从 `ground`（没给时是背景）升到自己的颜色，底色也一样。负的
/// 年龄、淡入之后、plain 模式都原样。终端自己颜色的文字升到主题的前景。
pub fn fade_in(
    theme: &Theme,
    spans: Vec<Span>,
    age: i64,
    ticks: i64,
    ground: Option<Color>,
) -> Vec<Span> {
    if age < 0 || age >= ticks || !theme.rich {
        return spans;
    }
    let p = age as f64 / ticks as f64;
    spans
        .into_iter()
        .map(|mut sp| {
            let st = sp.style;
            let fg = match st.fg_color() {
                Color::Default => theme.fg.map(Rgb::color),
                c => Some(c),
            };
            let with_fg = fg
                .and_then(|c| theme.rise(c, p, ground))
                .map_or(st, |c| st.with_fg(c));
            let with_bg = match st.bg_color() {
                Color::Default => with_fg,
                c => theme
                    .rise(c, p, ground)
                    .map_or(with_fg, |b| with_fg.with_bg(b)),
            };
            sp.style = with_bg;
            sp
        })
        .collect()
}

/// [`fade_in`] 用于 `(文字, 样式)` 的部分，去掉空的。
pub fn parts(
    theme: &Theme,
    parts: Vec<(String, Style)>,
    age: i64,
    ground: Option<Color>,
) -> Vec<Span> {
    let spans = parts
        .into_iter()
        .filter(|(t, _)| !t.is_empty())
        .map(|(t, st)| Span::new(t, st))
        .collect();
    fade_in(theme, spans, age, TICKS, ground)
}

/// 从 `from` 淡到 `to` 的一种墨色，过了 `age` 个 tick；结束之后是 `to`。
pub fn ink(theme: &Theme, from: Color, to: Style, age: i64, ticks: i64) -> Style {
    if age < 0 || age >= ticks {
        to
    } else {
        theme
            .blend(from, to.fg_color(), age as f64 / ticks as f64)
            .map_or(to, |c| to.with_fg(c))
    }
}
