//! 这一帧的笔（docs/puffin-tui.md §4.2「笔」）：工作光标，出现 `frame` 个 tick 之后。它交给它骑着的那一个条目。`still` 是
//! agent 的手停了多久（§4.2「笔」的「手停住」）：停满 [`lattice::GRACE_TICKS`] 之后点阵代替它。`held` 说有请求等你、而一批
//! 调用还在跑：这时只有那一批的页脚显示点阵。plain 模式保留旧的光标：闪烁的方块。
//! 移植自 packages/tui/tui/src/zcode/tui/shell/Pen.scala（feat/tui a2bf878b34）第 1–73 行。
//!
//! 与 Scala 的不同：`settled` 的位置是字节偏移，按码点往回走（spec §3）。
//! 从 Puffin 的壳（shell/pen.rs）搬进 kit（docs/specs/rust-cli/tui-kit.md §4.4），两套界面都调；行为不变。
use crate::app::element::{Element, map_lines};
use crate::app::widgets::lattice;
use crate::app::widgets::playhead::Playhead;
use crate::markdown::render::{CURSOR_GLYPH, SETTLED};
use crate::render::{Color, Span, Style, attr};
use crate::theme::ease::{self, Ink, java_space};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

/// 槽的宽度：一行开头为点阵与它后面的空格留的格数。
pub const SLOT: usize = 4;

/// 槽空着：四个空格。
pub fn blank() -> Vec<Span> {
    vec![Span::new(" ".repeat(SLOT), Style::DEFAULT)]
}

/// agent 写的字 `said` 之后的点阵，出现 `age` 个 tick：字紧跟着时先空一格，字后面已经是空白时就在那一格（思考的 ticker 与
/// 回答的末尾，§4.2「笔」的「手停住」）。
pub fn sign_after(theme: &Theme, said: &str, age: i64) -> Vec<Span> {
    let mut sign = match said.chars().last() {
        Some(c) if !java_space(c) => vec![Span::new(" ", Style::DEFAULT)],
        _ => Vec::new(),
    };
    sign.extend(lattice::sign(theme, age));
    sign
}

/// 手停住了多久（tick），与回合开始以来的毫秒。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Still {
    pub age: i64,
    pub turn_ms: f64,
}

/// 这一帧的笔：主题、呼吸到哪一帧、手停了多久、是否被请求扣住。
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Pen {
    pub theme: Theme,
    pub frame: i64,
    pub still: Option<Still>,
    pub held: bool,
}

impl Pen {
    /// 光标的呼吸：停住的笔尖与呼吸的 `○`（重试行、目标的检查、坞）用的墨色。
    pub fn caret(&self) -> Style {
        caret_style(&self.theme, self.frame)
    }

    /// 停着的笔尖：在它的墨色（brand，思考是 fg3）里呼吸的 `▌`；plain 模式是闪烁的方块。
    pub fn rest(&self, ink: Ink) -> Span {
        if !self.theme.rich {
            Span::new(CURSOR_GLYPH, self.caret())
        } else {
            Span::new(
                ease::NIB,
                breath_style(&self.theme, ease::nib_color(&self.theme, ink), self.frame),
            )
        }
    }

    /// 点阵出现了多久：手停住 [`lattice::GRACE_TICKS`] 或更久。
    pub fn sign_age(&self) -> Option<i64> {
        self.still
            .map(|s| s.age - lattice::GRACE_TICKS)
            .filter(|a| *a >= 0)
    }

    /// agent 等模型的地方：停着的笔尖，手停住 [`lattice::GRACE_TICKS`] 之后是工作行。
    pub fn waiting(&self) -> Vec<Span> {
        match self.sign_age() {
            None => vec![self.rest(Ink::Answer)],
            Some(age) => lattice::line(&self.theme, age, self.still.map_or(0.0, |s| s.turn_ms)),
        }
    }

    /// 说 agent 此刻在做什么的一行（一个调用、一批的页脚）的开头：四格，宽限期里空着，之后是点阵与一个空格。
    pub fn slot(&self) -> Vec<Span> {
        match self.sign_age() {
            None => blank(),
            Some(age) => {
                let mut s = lattice::sign(&self.theme, age);
                s.push(Span::new(" ", Style::DEFAULT));
                s
            }
        }
    }

    /// 点阵后面的那一段（调用的名称、名单末行的计数）：点阵在时，亮光离开点阵后接着走过第一段（[`lattice::band`]），
    /// 其余不变；宽限期里原样。
    pub fn band(&self, mut runs: Vec<Span>) -> Vec<Span> {
        match self.sign_age() {
            Some(age) if !runs.is_empty() => {
                let first = runs.remove(0);
                let mut out = lattice::band(&self.theme, age, first);
                out.extend(runs);
                out
            }
            _ => runs,
        }
    }

    /// 播放头 `p` 显示的字后面的笔尖：字落下时是实的，播放头停了 [`ease::REST_TICKS`] 之后停着呼吸。
    pub fn nib(&self, ink: Ink, p: &Playhead, now: i64) -> Span {
        if !self.theme.rich {
            Span::new(CURSOR_GLYPH, self.caret())
        } else if p.idle(now) > ease::REST_TICKS {
            self.rest(ink)
        } else {
            Span::new(ease::NIB, Style::fg(ease::nib_color(&self.theme, ink)))
        }
    }
}

/// 一轮最后一个字之后 `age` 个 tick 抬起的笔（§4.2「笔」的「抬笔」）；抬起之后或 plain 模式下没有。
pub fn lift(theme: &Theme, age: i64) -> Option<Span> {
    (theme.rich && (0..ease::LIFT_TICKS).contains(&age))
        .then(|| Span::new(ease::LIFT[age as usize], ease::lift_ink(theme)))
}

/// `inner` 里最新的字在落定，按播放头 `p` 落下它们的样子（§4.2「笔」的「落定」）：第 `k` 新的、不是空白的写出的字是
/// `text` 里 `shown` 之前第 `k` 个不是空白的码点。
pub fn settled(theme: &Theme, inner: Element, text: &str, p: &Playhead, now: i64) -> Element {
    // 以前（Marker）往回数 64 个字、按 20 个 tick 褪色：数错时旧字会一直带着底色。
    // 现在只数 16 个、4 个 tick 后一定是实的；空白两边都不数，折行丢掉的空格不会让它错开。
    if !theme.rich {
        return inner;
    }
    let mut ages = [SETTLED; ease::CELLS + 1];
    let end = p.shown.min(text.len());
    let mut k = 1;
    for (i, ch) in text[..end].char_indices().rev() {
        if k > ease::CELLS {
            break;
        }
        if !java_space(ch) {
            ages[k] = p.age_of(i, now).unwrap_or(SETTLED);
            k += 1;
        }
    }
    let theme = *theme;
    map_lines(inner, move |lines| {
        ease::settled(&theme, |j| ages.get(j).copied().unwrap_or(SETTLED), lines)
    })
}

/// 工作光标的一次呼吸，以 33 ms 的 tick 计：2.4 s。从壳的 `consts.rs` 搬来（壳再导出它）。
pub const BREATH_TICKS: i64 = 72;

/// `color` 像光标一样呼吸：向背景退去至多 [`Theme::BREATH_DEPTH`]（思考的笔尖在 fg3 里呼吸）；plain 模式同
/// [`caret_style`]。从壳的 `consts.rs` 搬来。
pub fn breath_style(theme: &Theme, color: Color, tick: i64) -> Style {
    let phase = tick.rem_euclid(BREATH_TICKS);
    let k = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * phase as f64 / BREATH_TICKS as f64).cos();
    match (color, theme.bg) {
        (Color::TrueColor(r, g, b), Some(bg)) if theme.rich => Style::fg(
            Rgb::new(r, g, b)
                .mix(bg, Theme::BREATH_DEPTH * (1.0 - k))
                .color(),
        ),
        _ => caret_style(theme, tick),
    }
}

/// 光标出现 `tick` 个 tick 之后的样子：从 brand 到向背景退去的 brand 再回来的余弦；plain 模式后半段换成 SGR dim（§4.2）。
/// 从壳的 `consts.rs` 搬来。
pub fn caret_style(theme: &Theme, tick: i64) -> Style {
    let phase = tick.rem_euclid(BREATH_TICKS);
    let k = 0.5 + 0.5 * (2.0 * std::f64::consts::PI * phase as f64 / BREATH_TICKS as f64).cos();
    match theme.breath(k) {
        Some(c) => Style::fg(c),
        None if phase < BREATH_TICKS / 2 => Style::fg(theme.brand),
        None => Style::fg(theme.brand).with_attr(attr::DIM),
    }
}
