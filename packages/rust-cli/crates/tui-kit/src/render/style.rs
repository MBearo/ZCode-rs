//! 样式：前景、背景与属性位打包进一个 `u64`，自己就是 intern 的键，布局时不带堆对象。
//! 移植自 packages/tui/tui/src/zcode/tui/render/Style.scala 与 StyledLine.scala 的 `Decor`（feat/tui a2bf878b34）。
//! 位布局与 Scala 相同：
//!
//! ```text
//!   bits  0..7   attrs（Written 在 bit 60）
//!   bits  8..9   fg kind（0 Default、1 Named、2 Indexed、3 TrueColor）
//!   bits 10..33  fg data
//!   bits 34..35  bg kind
//!   bits 36..59  bg data
//! ```
use super::ansi;
use super::color::Color;

/// SGR 属性位。
pub mod attr {
    pub const BOLD: u32 = 1;
    pub const DIM: u32 = 2;
    pub const ITALIC: u32 = 4;
    pub const UNDERLINE: u32 = 8;
    pub const REVERSE: u32 = 16;
    /// 不是 SGR：什么都不写，只标出输入框的光标格。渲染器把隐藏的终端光标停在一帧里第一个这样的格子上，输入法的
    /// 候选窗画在那里（docs/puffin-tui.md §4.4）。
    pub const CARET: u32 = 32;
    /// 删除线（SGR 9）。
    pub const STRIKE: u32 = 64;
    /// 不是 SGR，与 [`CARET`] 一样：这个样式里的空格不是折行的地方，所在的段（行内代码）放得下时整段移到下一行
    /// （docs/puffin-tui.md §4.2「回答的 Markdown」）。
    pub const WHOLE: u32 = 128;
    /// 不是 SGR：这个格子是代理写出的文字，笔把它算进最新落定的字（docs/puffin-tui.md §4.2「笔」）。在打包的样式里
    /// 占 bit 60：上面的八位都用完了。
    pub const WRITTEN: u32 = 256;
}

/// 一个视觉样式。`Default` 是没有颜色没有属性：一个普通的 SGR 重置。
#[derive(Clone, Copy, PartialEq, Eq, Hash, Default, Debug)]
pub struct Style(u64);

const FG_KIND_SHIFT: u32 = 8;
const FG_DATA_SHIFT: u32 = 10;
const BG_KIND_SHIFT: u32 = 34;
const BG_DATA_SHIFT: u32 = 36;
const KIND_MASK: u64 = 0x3;
const DATA_MASK: u64 = 0xff_ffff;
const WRITTEN_BIT: u64 = 1 << 60;

const fn pack_color(c: Color, kind_shift: u32, data_shift: u32) -> u64 {
    match c {
        Color::Default => 0,
        Color::Named(i) => (1 << kind_shift) | ((i as u64 & DATA_MASK) << data_shift),
        Color::Indexed(n) => (2 << kind_shift) | ((n as u64 & DATA_MASK) << data_shift),
        Color::TrueColor(r, g, b) => {
            let data = ((r as u64) << 16) | ((g as u64) << 8) | (b as u64);
            (3 << kind_shift) | ((data & DATA_MASK) << data_shift)
        }
    }
}

fn unpack_color(kind: u64, data: u64) -> Color {
    // 数据段按 kind 截断到 u8：打包时就是从 u8 来的。
    match kind {
        0 => Color::Default,
        1 => Color::Named((data & 0xff) as u8),
        2 => Color::Indexed((data & 0xff) as u8),
        _ => Color::TrueColor(
            ((data >> 16) & 0xff) as u8,
            ((data >> 8) & 0xff) as u8,
            (data & 0xff) as u8,
        ),
    }
}

impl Style {
    /// 没有颜色没有属性：一个普通的 SGR 重置。
    pub const DEFAULT: Style = Style(0);
    pub const BOLD: Style = Style::new(Color::Default, Color::Default, attr::BOLD);
    pub const DIM: Style = Style::new(Color::Default, Color::Default, attr::DIM);
    pub const ITALIC: Style = Style::new(Color::Default, Color::Default, attr::ITALIC);
    pub const UNDERLINE: Style = Style::new(Color::Default, Color::Default, attr::UNDERLINE);
    pub const REVERSE: Style = Style::new(Color::Default, Color::Default, attr::REVERSE);
    /// 输入框的光标格：反显，并标为光标。
    pub const CARET: Style =
        Style::new(Color::Default, Color::Default, attr::REVERSE | attr::CARET);

    pub const fn new(fg: Color, bg: Color, attrs: u32) -> Style {
        let written = if attrs & attr::WRITTEN != 0 {
            WRITTEN_BIT
        } else {
            0
        };
        Style(
            (attrs & 0xff) as u64
                | written
                | pack_color(fg, FG_KIND_SHIFT, FG_DATA_SHIFT)
                | pack_color(bg, BG_KIND_SHIFT, BG_DATA_SHIFT),
        )
    }

    pub const fn fg(c: Color) -> Style {
        Style::new(c, Color::Default, 0)
    }

    pub const fn bg(c: Color) -> Style {
        Style::new(Color::Default, c, 0)
    }

    pub const fn attrs_only(attrs: u32) -> Style {
        Style::new(Color::Default, Color::Default, attrs)
    }

    /// 打包的值：intern 的键。
    pub const fn raw(self) -> u64 {
        self.0
    }

    pub const fn attrs(self) -> u32 {
        (self.0 & 0xff) as u32
            | if self.0 & WRITTEN_BIT != 0 {
                attr::WRITTEN
            } else {
                0
            }
    }

    pub fn fg_color(self) -> Color {
        unpack_color(
            (self.0 >> FG_KIND_SHIFT) & KIND_MASK,
            (self.0 >> FG_DATA_SHIFT) & DATA_MASK,
        )
    }

    pub fn bg_color(self) -> Color {
        unpack_color(
            (self.0 >> BG_KIND_SHIFT) & KIND_MASK,
            (self.0 >> BG_DATA_SHIFT) & DATA_MASK,
        )
    }

    pub const fn has_attr(self, a: u32) -> bool {
        self.attrs() & a != 0
    }

    /// 叠加：属性位取并，`o` 的非默认颜色覆盖 `self` 的（外层的着色盖在内层的段上）。Scala 的 `++`。
    pub fn merge(self, o: Style) -> Style {
        let fg = if o.fg_color().is_default() {
            self.fg_color()
        } else {
            o.fg_color()
        };
        let bg = if o.bg_color().is_default() {
            self.bg_color()
        } else {
            o.bg_color()
        };
        Style::new(fg, bg, self.attrs() | o.attrs())
    }

    pub fn with_fg(self, c: Color) -> Style {
        Style::new(c, self.bg_color(), self.attrs())
    }

    pub fn with_bg(self, c: Color) -> Style {
        Style::new(self.fg_color(), c, self.attrs())
    }

    pub fn with_attr(self, a: u32) -> Style {
        Style::new(self.fg_color(), self.bg_color(), self.attrs() | a)
    }

    /// 从重置状态建立这个样式的 SGR，如 `CSI 0;1;36m`。自含（带前导的重置），与之前的样式无关。
    pub fn set_sequence(self) -> String {
        let mut out = String::with_capacity(16);
        self.append_set_sequence(&mut out);
        out
    }

    pub fn append_set_sequence(self, out: &mut String) {
        out.push_str(ansi::CSI);
        out.push('0');
        let a = self.attrs();
        if a & attr::BOLD != 0 {
            out.push_str(";1");
        }
        if a & attr::DIM != 0 {
            out.push_str(";2");
        }
        if a & attr::ITALIC != 0 {
            out.push_str(";3");
        }
        if a & attr::UNDERLINE != 0 {
            out.push_str(";4");
        }
        if a & attr::REVERSE != 0 {
            out.push_str(";7");
        }
        if a & attr::STRIKE != 0 {
            out.push_str(";9");
        }
        self.fg_color().append_sgr(out, true);
        self.bg_color().append_sgr(out, false);
        out.push('m');
    }
}

/// 一段文字在 SGR [`Style`] 之外带的东西，样式里放不下：下划线的颜色（SGR 58；`Default` 用终端的）与
/// OSC 8 链接的目标（空串是没有）。
#[derive(Clone, Debug, PartialEq, Eq, Hash, Default)]
pub struct Decor {
    pub underline: Color,
    pub link: String,
}

impl Decor {
    pub const PLAIN: Decor = Decor {
        underline: Color::Default,
        link: String::new(),
    };

    pub fn underline(c: Color) -> Decor {
        Decor {
            underline: c,
            link: String::new(),
        }
    }

    pub fn link(target: impl Into<String>) -> Decor {
        Decor {
            underline: Color::Default,
            link: target.into(),
        }
    }

    pub fn is_plain(&self) -> bool {
        self.underline.is_default() && self.link.is_empty()
    }

    /// 设下划线颜色的 SGR，冒号形式，接在样式自己的 SGR 之后：不认识它的终端只丢这一段，不丢前面的颜色。
    pub fn sequence(&self) -> String {
        match self.underline {
            Color::TrueColor(r, g, b) => format!("{}58:2::{r}:{g}:{b}m", ansi::CSI),
            Color::Indexed(n) => format!("{}58:5:{n}m", ansi::CSI),
            Color::Named(i) => format!("{}58:5:{i}m", ansi::CSI),
            Color::Default => String::new(),
        }
    }
}
