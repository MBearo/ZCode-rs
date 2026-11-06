//! 设计 token（docs/puffin-tui.md §4.2）。回答的正文不带颜色：它是终端的默认前景。Terminal 主题的一切从终端的前景与
//! 背景派生；具名主题（[`Scheme`]）从它的种子派生；终端不报告颜色时是 plain，退到 SGR dim 与 ANSI 调色板。视图只读
//! token，不知道是哪个主题做的。
//! 移植自 packages/tui/tui/src/zcode/tui/Theme.scala（feat/tui a2bf878b34；`Rgb` 在 `rgb.rs`）。
//!
//! 画面门禁按格子比较 Scala 与 Rust 的画面，所以每个派生都照 Scala 的算式逐步写：同样的 f64 运算次序、同样的循环
//! 累加（`t += 0.05` 而不是 `i * 0.05`），结果逐位相同。
// 模块名与上级 `theme` 相同是 mod.rs 的分工（token 在 `theme::theme`）；外面经 `theme::theme::Theme` 用它。
#![allow(clippy::module_inception)]

use crate::render::{Color, Style};

use super::rgb::Rgb;
use super::scheme::{Ground, Scheme, ThemeChoice, ThemeMode};
use super::schemes;

/// 代码块各语法角色的墨（docs/puffin-tui.md §4.2「回答的 Markdown」）；注释是 fg3 斜体，其余是正文。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Syntax {
    pub keyword: Color,
    pub string: Color,
    pub number: Color,
    pub function: Color,
    /// 类型名（Scala 的 `typ`）。
    pub ty: Color,
    pub constant: Color,
    pub key: Color,
}

impl Syntax {
    /// plain 模式：代码块没有颜色。
    pub const PLAIN: Syntax = Syntax {
        keyword: Color::Default,
        string: Color::Default,
        number: Color::Default,
        function: Color::Default,
        ty: Color::Default,
        constant: Color::Default,
        key: Color::Default,
    };
}

/// 设计 token。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Theme {
    pub rich: bool,
    /// 地的前景，只在 rich 模式下知道；流式的尾巴与标志向它混合。
    pub fg: Option<Rgb>,
    /// 地的背景，只在 rich 模式下知道。
    pub bg: Option<Rgb>,
    pub fg2: Style,
    pub fg3: Style,
    pub line: Style,
    /// 板与代码面板的底色；plain 模式没有（不铺底色）。
    pub panel: Option<Color>,
    pub brand: Color,
    pub code: Color,
    pub ok: Color,
    pub err: Color,
    /// `$` 提及的技能（docs/puffin-tui.md §4.2「提及技能」）。
    pub skill: Color,
    /// `@` 提及的文件或文件夹（docs/puffin-tui.md §4.2「提及（@）」）。
    pub file: Color,
    /// `@` 提及的过去的工作：一个会话或一次工作流运行。
    pub work: Color,
    /// `@` 提及的子代理。
    pub agent: Color,
    /// 成功结束的命令的输出：比 fg3 更安静（docs/puffin-tui.md §4.2「工具调用」）。
    pub quiet: Style,
    /// 新增与删除的 diff 行的底色；plain 模式没有。
    pub add_bg: Option<Color>,
    pub del_bg: Option<Color>,
    /// 回答里的行内代码：偏冷的灰，plain 模式是正文（docs/puffin-tui.md §4.2「回答的 Markdown」）。
    pub code_ink: Style,
    /// 行内代码两边的反引号：code_ink 向背景混合一半，plain 模式是 dim。
    pub tick: Style,
    /// 代码块的语法墨；plain 模式不用。
    pub syntax: Syntax,
    /// 主题自己涂地：[`Theme::fg`] 与 [`Theme::bg`] 填满每个没有颜色的格子与终端的边距
    /// （docs/puffin-tui.md §4.2「谁涂地」）。
    pub paints: bool,
}

impl Theme {
    /// 光标呼吸到最低处时向背景退多远：变暗，但从不接近消失。
    pub const BREATH_DEPTH: f64 = 0.45;
    /// 流式文字里的新字从离背景多远处开始。
    pub const STREAM_DEPTH: f64 = 0.55;
    /// fg2 与 brand 在 bg 与 panel 上的最低 WCAG 对比度。
    pub const FLOOR_TEXT: f64 = 4.5;
    /// fg3、其余墨色角色与语法墨的最低对比度。
    pub const FLOOR_INK: f64 = 3.0;

    /// 没有颜色信息：较安静的墨用 dim，强调色用 ANSI，没有底色。
    pub const PLAIN: Theme = Theme {
        rich: false,
        fg: None,
        bg: None,
        fg2: Style::DIM,
        fg3: Style::DIM,
        line: Style::DIM,
        panel: None,
        brand: Color::BLUE,
        code: Color::YELLOW,
        ok: Color::GREEN,
        err: Color::RED,
        skill: Color::MAGENTA,
        file: Color::BLUE,
        work: Color::CYAN,
        agent: Color::GREEN,
        quiet: Style::DIM,
        add_bg: None,
        del_bg: None,
        code_ink: Style::DEFAULT,
        tick: Style::DIM,
        syntax: Syntax::PLAIN,
        paints: false,
    };

    /// 报告了颜色、至少 256 色的终端上的 Terminal 主题。
    pub fn from(fg: Rgb, bg: Rgb) -> Theme {
        let dark = bg.lightness() < 0.5;
        // 强调色同亮度同彩度、只差色相；深色背景取更亮的一档。
        let (l, c) = if dark { (0.76, 0.13) } else { (0.56, 0.14) };
        let accent = |h: f64| Rgb::oklch(l, c, h).color();
        // 行内代码：brand 的色相、正文的亮度、很低的彩度，读起来是偏冷的正文色。
        // 浅色背景上低亮度的色要多一点彩度才看得出同样的冷暖。
        let code_ink = Rgb::oklch(fg.lightness(), if dark { 0.035 } else { 0.055 }, 237.0);
        // 差异行的底色：强调色向背景混合，深色背景 86%、浅色 87%。
        let diff = if dark { 0.86 } else { 0.87 };
        Theme {
            rich: true,
            fg: Some(fg),
            bg: Some(bg),
            fg2: Style::fg(fg.mix(bg, 0.32).color()),
            fg3: Style::fg(fg.mix(bg, 0.52).color()),
            line: Style::fg(fg.mix(bg, 0.8).color()),
            panel: Some(fg.mix(bg, 0.94).color()),
            brand: accent(237.0),
            code: accent(62.0),
            ok: accent(150.0),
            err: accent(25.0),
            skill: accent(300.0),
            // @ 提及的三种颜色：文件与 brand 同一色相，过去的工作是青色，子代理是橄榄绿。
            file: accent(237.0),
            work: accent(190.0),
            agent: accent(112.0),
            quiet: Style::fg(fg.mix(bg, 0.64).color()),
            add_bg: Some(Rgb::oklch(l, c, 150.0).mix(bg, diff).color()),
            del_bg: Some(Rgb::oklch(l, c, 25.0).mix(bg, diff).color()),
            code_ink: Style::fg(code_ink.color()),
            tick: Style::fg(code_ink.mix(bg, 0.5).color()),
            syntax: Syntax {
                keyword: accent(300.0),
                string: accent(150.0),
                number: accent(62.0),
                function: accent(237.0),
                ty: accent(190.0),
                constant: accent(25.0),
                key: accent(112.0),
            },
            paints: false,
        }
    }

    /// 按终端告诉我们的选主题，选择是默认的（Terminal）。rich 要两个颜色都有、而且终端至少 256 色（`colorful`）；否则是
    /// plain。
    pub fn choose(fg: Option<Rgb>, bg: Option<Rgb>, colorful: bool) -> Theme {
        Theme::resolve(fg, bg, colorful, &ThemeChoice::default(), None)
    }

    /// `choice` 在颜色为 `fg`、`bg` 的终端上给出的主题（docs/puffin-tui.md §4.2「主题」）。`appearance` 是终端最近的
    /// DEC 2031 报告（`true` 是深色）；`auto` 下它为涂地的主题选侧。地是终端的主题按终端的颜色走，调用方在报告之后
    /// 再问一次颜色。终端不是 rich 时总是 plain，不论选了什么。
    ///
    /// `colorful`：终端至少 256 色（docs/specs/rust-cli/tui-kit.md §3.4，`ColorDepth::colorful`）。主题的颜色总是真彩，
    /// 256 色终端上由渲染器降采样（`Renderer::with_depth`）；16 色终端是 plain。以前这个参数是"真彩终端"。
    pub fn resolve(
        fg: Option<Rgb>,
        bg: Option<Rgb>,
        colorful: bool,
        choice: &ThemeChoice,
        appearance: Option<bool>,
    ) -> Theme {
        match (fg, bg) {
            (Some(f), Some(b)) if colorful => match schemes::by_id(choice.theme) {
                None => Theme::from(f, b),
                Some(s) => {
                    let paints = choice.ground == Ground::Theme;
                    let dark = match choice.mode {
                        ThemeMode::Light => false,
                        ThemeMode::Dark => true,
                        ThemeMode::Auto => appearance
                            .filter(|_| paints)
                            .unwrap_or_else(|| b.lightness() < 0.5),
                    };
                    Theme::scheme(s, dark, if paints { None } else { Some((f, b)) })
                }
            },
            _ => Theme::PLAIN,
        }
    }

    /// 具名主题的一侧作为 token：在它自己的地上，或 `terminal`（fg, bg）给出时在终端的地上，只用它的墨。
    pub fn scheme(s: &Scheme, dark: bool, terminal: Option<(Rgb, Rgb)>) -> Theme {
        let p = s.side(dark).palette;
        let (fg, bg) = terminal.unwrap_or((p.fg, p.bg));
        let dark_ground = bg.lightness() < 0.5;
        let panel = if terminal.is_some() {
            fg.mix(bg, 0.94)
        } else {
            Theme::band(p.surface, fg, bg)
        };
        let grounds = [bg, panel];
        // fg3 是配色自己的注释色：作者为「退后一步」设计的颜色；地是终端的时候没有它，用 Terminal 的混合。
        let grey3 = if terminal.is_some() {
            fg.mix(bg, 0.52)
        } else {
            p.muted
        };
        let fg3 = Theme::lift_grey(grey3, fg, &grounds, Theme::FLOOR_INK);
        let fg2 = Theme::lift_grey(fg.mix(fg3, 0.45), fg, &grounds, Theme::FLOOR_TEXT);
        // 角色表的每个名字在两侧都有墨（schemes_tests 检查）；缺了按 fg 抬，不会发生。
        let ink_at = |name: &str, floor: f64| {
            Theme::lift(p.ink(name).unwrap_or(fg), &grounds, floor, dark_ground)
        };
        let ink = |name: &str| ink_at(name, Theme::FLOOR_INK);
        let r = s.roles;
        let brand = ink_at(r.brand, Theme::FLOOR_TEXT);
        let (ok, err) = (ink(r.ok), ink(r.err));
        // 行内代码：brand 的色相、字的亮度、很低的彩度，与 Terminal 的算法相同。
        let code_ink = Rgb::oklch(
            fg.lightness(),
            if dark_ground { 0.035 } else { 0.055 },
            brand.to_oklch().2,
        );
        let diff = if dark_ground { 0.86 } else { 0.87 };
        let y = s.syntax;
        Theme {
            rich: true,
            fg: Some(fg),
            bg: Some(bg),
            fg2: Style::fg(fg2.color()),
            fg3: Style::fg(fg3.color()),
            line: Style::fg(fg3.mix(bg, 0.58).color()),
            panel: Some(panel.color()),
            brand: brand.color(),
            code: ink(r.code).color(),
            ok: ok.color(),
            err: err.color(),
            skill: ink(r.skill).color(),
            file: ink(r.file).color(),
            work: ink(r.work).color(),
            agent: ink(r.agent).color(),
            quiet: Style::fg(fg3.mix(bg, 0.25).color()),
            add_bg: Some(ok.mix(bg, diff).color()),
            del_bg: Some(err.mix(bg, diff).color()),
            code_ink: Style::fg(code_ink.color()),
            tick: Style::fg(code_ink.mix(bg, 0.5).color()),
            syntax: Syntax {
                keyword: ink(y.keyword).color(),
                string: ink(y.string).color(),
                number: ink(y.number).color(),
                function: ink(y.function).color(),
                ty: ink(y.ty).color(),
                constant: ink(y.constant).color(),
                key: ink(y.key).color(),
            },
            paints: terminal.is_none(),
        }
    }

    /// 板：配色的 surface，与 bg 的 OKLab 亮度差保持在 0.025–0.06。太近看不出时用 fg 向 bg 混合 94%；太远、压过字时
    /// 以 5% 一步向 bg 退，但不走会小于 0.025 的那一步：纯黑附近没有落在带里的颜色（`#010101` 与 `#000000` 差
    /// 0.067），板仍然要看得出。
    pub fn band(surface: Rgb, fg: Rgb, bg: Rgb) -> Rgb {
        let bl = bg.lightness();
        let d = |c: Rgb| (c.lightness() - bl).abs();
        if d(surface) < 0.025 {
            return fg.mix(bg, 0.94);
        }
        let mut t = 0.0;
        let mut out = surface;
        while d(out) > 0.06 && t < 1.0 {
            t += 0.05;
            let next = surface.mix(bg, t);
            if d(next) < 0.025 {
                break;
            }
            out = next;
        }
        out
    }

    /// `ink` 的 OKLCH 亮度一步 0.01 地移动（深色地上变亮，浅色地上变暗），直到它在 `grounds` 的每一个上都达到
    /// `floor`；色相与彩度不变，所以仍是配色的那个颜色。
    pub fn lift(ink: Rgb, grounds: &[Rgb], floor: f64, dark: bool) -> Rgb {
        if worst(ink, grounds) >= floor {
            return ink;
        }
        let (l0, c, h) = ink.to_oklch();
        let step = if dark { 0.01 } else { -0.01 };
        let mut l = l0;
        let mut out = ink;
        let mut i = 0;
        while i < 60 && worst(out, grounds) < floor {
            l = (l + step).clamp(0.0, 1.0);
            out = Rgb::oklch(l, c, h);
            i += 1;
        }
        out
    }

    /// 一个灰一步 4% 地向 `fg` 移动，直到它在 `grounds` 的每一个上都达到 `floor`。
    pub fn lift_grey(grey: Rgb, fg: Rgb, grounds: &[Rgb], floor: f64) -> Rgb {
        let mut t = 0.0;
        let mut out = grey;
        while worst(out, grounds) < floor && t < 1.0 {
            t += 0.04;
            out = grey.mix(fg, t);
        }
        out
    }

    /// `from` 缓入 `to`，走了 `p`（ease-out，与 [`Theme::tint`] 同一条曲线）：淡入淡出与换墨。plain 模式没有混合。
    pub fn blend(&self, from: Color, to: Color, p: f64) -> Option<Color> {
        match (from, to) {
            (Color::TrueColor(r, g, b), Color::TrueColor(tr, tg, tb)) if self.rich => {
                let x = p.clamp(0.0, 1.0);
                let t = 1.0 - (1.0 - x) * (1.0 - x);
                Some(Rgb::new(r, g, b).mix(Rgb::new(tr, tg, tb), t).color())
            }
            _ => None,
        }
    }

    /// 新显示的字在淡入中走了 `p`（0 刚显示，1 落定）：`target` 自己的颜色，从向背景退 [`Theme::STREAM_DEPTH`]
    /// 处升到它自己（docs/puffin-tui.md §4.2，流式的节奏）。它从不经过 brand：光标已经标出了书写的边。plain 模式
    /// 没有染色。
    pub fn tint(&self, target: Color, p: f64) -> Option<Color> {
        match (self.bg, target) {
            (Some(b), Color::TrueColor(r, g, bl)) if self.rich => {
                let start = Rgb::new(r, g, bl).mix(b, Theme::STREAM_DEPTH).color();
                self.blend(start, target, p)
            }
            _ => None,
        }
    }

    /// 新东西淡入走了 `p`（docs/puffin-tui.md §4.2「动效的原则」）：`target` 从它画在其上的 `ground` 升起（没给时是
    /// 背景，板上是 panel），同样的 ease-out。plain 模式没有淡入。
    pub fn rise(&self, target: Color, p: f64, ground: Option<Color>) -> Option<Color> {
        ground
            .or_else(|| self.bg.map(Rgb::color))
            .and_then(|g| self.blend(g, target, p))
    }

    /// 书写光标在呼吸的 `k` 处的颜色：1 是 brand，0 是 brand 向背景混合 45%（docs/puffin-tui.md §4.2）。plain 模式
    /// 没有混合：调用方改用 SGR dim。
    pub fn breath(&self, k: f64) -> Option<Color> {
        match (self.bg, self.brand) {
            (Some(b), Color::TrueColor(r, g, bl)) => Some(
                Rgb::new(r, g, bl)
                    .mix(b, Theme::BREATH_DEPTH * (1.0 - k))
                    .color(),
            ),
            _ => None,
        }
    }
}

/// `c` 在 `grounds` 上最差的对比度。
fn worst(c: Rgb, grounds: &[Rgb]) -> f64 {
    grounds
        .iter()
        .map(|&g| c.contrast(g))
        .fold(f64::INFINITY, f64::min)
}
