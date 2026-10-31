//! 配色记号（docs/specs/rust-cli/tui.md §7.4，修复线 F26，用户决定）：次要文字、按键、选中行、模型名与工作区路径。
//! 颜色只取决于启动时探测到的终端事实（`Palette`）：前景、背景、色深与明暗。
use ratatui::style::{Color, Modifier, Style};

use super::palette::{BLACK, ColorLevel, Palette, Rgb, WHITE, blend, is_light};

// 配色的命名常量（tui.md §7.4）：别处要同样的颜色时用这里的常量与 `Palette` 的方法，不另写字面量（F27 的 footer
// 右侧统计复用它们）。

/// 文字与背景的最低对比度（WCAG AA 的 4.5:1）。
pub const MIN_TEXT_CONTRAST: f64 = 4.5;
/// 次要文字：前景在背景上的比例。
pub const SECONDARY_ALPHA: f64 = 0.6;
/// 浅色背景上按键的前景比例。
pub const KEY_ALPHA_LIGHT: f64 = 0.85;
/// 选中行的底色：深色背景用较深的蓝（BLUE_200），浅色背景用较浅的蓝（BLUE_100）。
pub const SELECTION_FILL_DARK: Rgb = (99, 168, 248);
pub const SELECTION_FILL_LIGHT: Rgb = (164, 205, 251);
/// 强调色：深色背景就是选中行的深色底色，浅色背景另取深一些的蓝；
/// 16 色终端用 `ACCENT_NAMED`。列表与面板顶上的线和标题、工作区信任对话框（F26 起不再用黄色）都用它。
pub const ACCENT_DARK: Rgb = SELECTION_FILL_DARK;
pub const ACCENT_LIGHT: Rgb = (28, 100, 200);
pub const ACCENT_NAMED: Color = Color::Cyan;
/// 选中行的文字：接近黑色的深蓝，在蓝色底色上读得清。
pub const SELECTION_TEXT: Rgb = (0, 0, 46);
/// 底色与背景的对比度低于这个值时换另一种蓝（底色与背景分不开时）。
pub const SELECTION_MIN_RATIO: f64 = 1.25;
/// 状态栏颜色的柔化：原色占的百分比。
pub const STATUS_SATURATION_PERCENT: u32 = 85;
/// 模型名：catppuccin mocha / latte 的类型色；16 色终端用 `MODEL_NAMED`。
pub const MODEL_DARK: Rgb = (249, 226, 175);
pub const MODEL_LIGHT: Rgb = (223, 142, 29);
pub const MODEL_NAMED: Color = Color::Cyan;
/// 工作区路径：catppuccin 的字符串色；16 色终端用 `PATH_NAMED`。
pub const PATH_DARK: Rgb = (166, 227, 161);
pub const PATH_LIGHT: Rgb = (64, 160, 43);
pub const PATH_NAMED: Color = Color::Green;
/// footer 右侧“上下文快满”且自动压缩关闭时的颜色（compact.md §2.1 的 `Context low`）。
pub const CONTEXT_LOW: Color = Color::Red;
/// 查看子代理时输入框的边线与标签的底色（修复线 F62，tui-panels.md §1.6、tui.md §7.4）：深浅两种背景都是这个青色；
/// 16 色终端深色背景用亮青、浅色背景用青。
pub const SUBAGENT: Rgb = (8, 145, 178);
pub const SUBAGENT_NAMED_DARK: Color = Color::LightCyan;
pub const SUBAGENT_NAMED_LIGHT: Color = Color::Cyan;
/// 标签的字：与终端文字相反的颜色（深色背景黑、浅色背景白）。不按 [`MIN_TEXT_CONTRAST`] 调：白字与底色 3.7:1，
/// 调了就成了黑字，与用户要的截图不同（tui.md §7.4）。
pub const SUBAGENT_TEXT_DARK: Rgb = (0, 0, 0);
pub const SUBAGENT_TEXT_LIGHT: Rgb = (255, 255, 255);
pub const SUBAGENT_TEXT_NAMED_DARK: Color = Color::Black;
pub const SUBAGENT_TEXT_NAMED_LIGHT: Color = Color::White;
/// 成功色（工作线 F74，tui.md §7.4、§7.6）：提问视图里勾上的 `[✔]`、单选已选项与确认页的回答。深浅背景各一种绿，
/// 不按对比度调；16 色终端深色背景用亮绿、浅色背景用绿。
pub const SUCCESS_DARK: Rgb = (78, 186, 101);
pub const SUCCESS_LIGHT: Rgb = (44, 122, 57);
pub const SUCCESS_NAMED_DARK: Color = Color::LightGreen;
pub const SUCCESS_NAMED_LIGHT: Color = Color::Green;

/// 自主模式（guarded）的名称：App 的 `--color-icon-blue`（= `--color-terminal-bright-blue`，`packages/ui/src/styles.css`）。
/// 浅色主题 sky-500、深色主题 sky-600（Tailwind 4 的 oklch 换成 sRGB）；不按对比度调（App 也不调）。16 色终端用蓝。
/// 工作线 F76（用户决定 2026-10-23，tui.md §7.4）。
pub const ICON_BLUE_LIGHT: Rgb = (0, 166, 244);
pub const ICON_BLUE_DARK: Rgb = (0, 132, 209);
pub const ICON_BLUE_NAMED: Color = Color::Blue;
/// 完全访问（yolo）的名称：App 的 `--color-warning`，浅色主题 yellow-600、深色主题 yellow-500；16 色终端用黄。
pub const WARNING_LIGHT: Rgb = (208, 135, 0);
pub const WARNING_DARK: Rgb = (240, 177, 0);
pub const WARNING_NAMED: Color = Color::Yellow;
/// 切到完全访问的确认框里勾选之后的“确认启用”：App 的 `--color-destructive`（确认按钮是 `variant="destructive"`），
/// 浅色主题 red-600、深色主题 red-500；16 色终端用红。工作线 F87（用户决定 2026-10-24，tui.md §7.4、§7.8）。
pub const DESTRUCTIVE_LIGHT: Rgb = (231, 0, 11);
pub const DESTRUCTIVE_DARK: Rgb = (251, 44, 54);
pub const DESTRUCTIVE_NAMED: Color = Color::Red;
/// 搜索模式的当前匹配（修复线 F114，tui.md §7.4）：底色复用 [`WARNING_DARK`] / [`WARNING_LIGHT`]，字黑色，粗体加下划线；
/// 16 色终端黄底黑字。
pub const FIND_CURRENT_TEXT: Rgb = (0, 0, 0);
pub const FIND_CURRENT_NAMED: Color = Color::Yellow;
pub const FIND_CURRENT_TEXT_NAMED: Color = Color::Black;

// WCAG 2.x 的相对亮度与对比度：sRGB 分量先线性化（不超过 0.04045 时除以 12.92，否则
// `((v + 0.055) / 1.055)^2.4`），再按 Rec. 709 的权重加权；对比度是 `(亮 + 0.05) / (暗 + 0.05)`。
const SRGB_LINEAR_LIMIT: f64 = 0.04045;
const SRGB_LINEAR_SLOPE: f64 = 12.92;
const SRGB_OFFSET: f64 = 0.055;
const SRGB_SCALE: f64 = 1.055;
const SRGB_GAMMA: f64 = 2.4;
const LUMINANCE_WEIGHTS: [f64; 3] = [0.2126, 0.7152, 0.0722];
const CONTRAST_FLARE: f64 = 0.05;
/// 状态行颜色的整数亮度：权重（红、绿、蓝）之和是 256。
const STATUS_LUMA_WEIGHTS: [u32; 3] = [77, 150, 29];
const STATUS_LUMA_SCALE: u32 = 256;
/// 百分比的分母（整数运算）。
const PERCENT: u32 = super::quantities::PERCENT as u32;

fn luminance((r, g, b): Rgb) -> f64 {
    let channel = |value: u8| {
        let value = f64::from(value) / f64::from(u8::MAX);
        if value <= SRGB_LINEAR_LIMIT {
            value / SRGB_LINEAR_SLOPE
        } else {
            ((value + SRGB_OFFSET) / SRGB_SCALE).powf(SRGB_GAMMA)
        }
    };
    let [wr, wg, wb] = LUMINANCE_WEIGHTS;
    wr * channel(r) + wg * channel(g) + wb * channel(b)
}

/// 两种颜色的对比度（WCAG）。
pub fn ratio(a: Rgb, b: Rgb) -> f64 {
    let (a, b) = (luminance(a), luminance(b));
    (a.max(b) + CONTRAST_FLARE) / (a.min(b) + CONTRAST_FLARE)
}

/// 在 `background` 上读得清的 `preferred`（真彩色）：对比度够就用原色，
/// 否则向黑或白（对比度大的那个）靠，取第一个够 4.5:1 的颜色。
pub fn readable(preferred: Rgb, background: Rgb) -> Rgb {
    if ratio(preferred, background) >= MIN_TEXT_CONTRAST {
        return preferred;
    }
    let endpoint = if ratio(BLACK, background) >= ratio(WHITE, background) {
        BLACK
    } else {
        WHITE
    };
    (1..=u8::MAX)
        .map(|step| blend(preferred, endpoint, f64::from(step) / f64::from(u8::MAX)))
        .find(|candidate| ratio(*candidate, background) >= MIN_TEXT_CONTRAST)
        .unwrap_or(endpoint)
}

/// 状态栏颜色的柔化：85% 原色加 15% 亮度。
fn soften((r, g, b): Rgb) -> Rgb {
    let [wr, wg, wb] = STATUS_LUMA_WEIGHTS;
    let luma = (wr * u32::from(r) + wg * u32::from(g) + wb * u32::from(b)) / STATUS_LUMA_SCALE;
    let channel = |c: u8| {
        ((u32::from(c) * STATUS_SATURATION_PERCENT
            + luma * (PERCENT - STATUS_SATURATION_PERCENT)
            + PERCENT / 2)
            / PERCENT)
            .min(u32::from(u8::MAX)) as u8
    };
    (channel(r), channel(g), channel(b))
}

impl Palette {
    /// 前景与背景都知道、并且前景与背景的明暗相反时的（前景，背景）；只知道一个时不算。
    fn pair(&self) -> Option<(Rgb, Rgb)> {
        let background = self.background?;
        let foreground = self.foreground?;
        (is_light(foreground) != self.light).then_some((foreground, background))
    }

    /// 次要文字：`blend(前景, 背景, 0.6)`，调到 4.5:1；颜色不知道或 16 色时 dim。
    pub fn secondary(&self) -> Style {
        let color = self
            .pair()
            .and_then(|(fg, bg)| self.tint(readable(blend(bg, fg, SECONDARY_ALPHA), bg)));
        match color {
            Some(color) => Style::default()
                .fg(color)
                .remove_modifier(Modifier::DIM | Modifier::BOLD),
            None => Style::default().add_modifier(Modifier::DIM),
        }
    }

    /// 按键：粗体、不 dim；浅色背景上前景是 `blend(前景, 背景, 0.85)`，否则终端的前景色。
    pub fn key(&self) -> Style {
        let style = Style::default()
            .add_modifier(Modifier::BOLD)
            .remove_modifier(Modifier::DIM);
        match self.pair() {
            Some((fg, bg)) if self.light => self
                .tint(readable(blend(bg, fg, KEY_ALPHA_LIGHT), bg))
                .map_or(style, |color| style.fg(color)),
            _ => style,
        }
    }

    /// 选中行：整行铺蓝色底色，文字 (0,0,46) 按底色调对比度、粗体；
    /// 背景不知道或 16 色时反色加粗。
    pub fn selection(&self) -> Style {
        let fallback = Style::default().add_modifier(Modifier::BOLD | Modifier::REVERSED);
        let Some(background) = self.background else {
            return fallback;
        };
        if self.level == ColorLevel::Ansi16 {
            return fallback;
        }
        let (mut fill, alternate) = if self.light {
            (SELECTION_FILL_LIGHT, SELECTION_FILL_DARK)
        } else {
            (SELECTION_FILL_DARK, SELECTION_FILL_LIGHT)
        };
        // 底色太接近背景（蓝色的终端）时换另一种蓝。
        if ratio(fill, background) < SELECTION_MIN_RATIO
            && ratio(alternate, background) > ratio(fill, background)
        {
            fill = alternate;
        }
        match (self.tint(readable(SELECTION_TEXT, fill)), self.tint(fill)) {
            (Some(fg), Some(bg)) => Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
            _ => fallback,
        }
    }

    /// 模型名：主题的类型色，柔化后调对比度；16 色终端是青色。
    pub fn model(&self) -> Style {
        self.status_line(MODEL_DARK, MODEL_LIGHT, MODEL_NAMED)
    }

    /// 工作区路径：主题的字符串色，柔化后调对比度；16 色终端是绿色。
    pub fn path(&self) -> Style {
        self.status_line(PATH_DARK, PATH_LIGHT, PATH_NAMED)
    }

    /// footer 右侧“上下文”提示（compact.md §2.1）：快满且自动压缩关闭时 [`CONTEXT_LOW`]，其余是次要文字。
    pub fn context_notice(&self, low: bool) -> Style {
        if low {
            Style::default().fg(CONTEXT_LOW)
        } else {
            self.secondary()
        }
    }

    /// 查看子代理时输入框的上下边线（修复线 F62）：[`SUBAGENT`]，16 色终端按背景取亮青或青。
    pub fn subagent_rule(&self) -> Style {
        Style::default().fg(self.subagent_color())
    }

    /// 查看子代理时上边线右侧的标签（修复线 F62）：[`SUBAGENT`] 的底色，反色的字（深色背景黑、浅色背景白）。
    pub fn subagent_label(&self) -> Style {
        let (text, named) = if self.light {
            (SUBAGENT_TEXT_LIGHT, SUBAGENT_TEXT_NAMED_LIGHT)
        } else {
            (SUBAGENT_TEXT_DARK, SUBAGENT_TEXT_NAMED_DARK)
        };
        Style::default()
            .fg(self.tint(text).unwrap_or(named))
            .bg(self.subagent_color())
    }

    fn subagent_color(&self) -> Color {
        let named = if self.light {
            SUBAGENT_NAMED_LIGHT
        } else {
            SUBAGENT_NAMED_DARK
        };
        self.tint(SUBAGENT).unwrap_or(named)
    }

    /// 成功色（[`SUCCESS_DARK`] / [`SUCCESS_LIGHT`]），16 色终端按背景取亮绿或绿。
    pub fn success(&self) -> Style {
        let (rgb, named) = if self.light {
            (SUCCESS_LIGHT, SUCCESS_NAMED_LIGHT)
        } else {
            (SUCCESS_DARK, SUCCESS_NAMED_DARK)
        };
        Style::default().fg(self.tint(rgb).unwrap_or(named))
    }

    /// 自主模式的颜色（[`ICON_BLUE_LIGHT`] / [`ICON_BLUE_DARK`]），16 色终端是蓝。
    pub fn icon_blue(&self) -> Style {
        self.theme_color(ICON_BLUE_LIGHT, ICON_BLUE_DARK, ICON_BLUE_NAMED)
    }

    /// 完全访问的颜色（[`WARNING_LIGHT`] / [`WARNING_DARK`]），16 色终端是黄。
    pub fn warning(&self) -> Style {
        self.theme_color(WARNING_LIGHT, WARNING_DARK, WARNING_NAMED)
    }

    /// 确认框里可用的“确认启用”（[`DESTRUCTIVE_LIGHT`] / [`DESTRUCTIVE_DARK`]），16 色终端是红。
    pub fn destructive(&self) -> Style {
        self.theme_color(DESTRUCTIVE_LIGHT, DESTRUCTIVE_DARK, DESTRUCTIVE_NAMED)
    }

    /// 搜索的匹配（主界面的搜索模式与详细视图的 `/`，修复线 F114）：反色，深浅背景、各颜色级别相同。
    pub fn find_match(&self) -> Style {
        Style::default().add_modifier(Modifier::REVERSED)
    }

    /// 搜索模式的当前匹配：警告色的底、黑字，粗体加下划线；叠在 [`Self::find_match`] 上时去掉反色。
    pub fn find_current(&self) -> Style {
        let fill = if self.light {
            WARNING_LIGHT
        } else {
            WARNING_DARK
        };
        Style::default()
            .fg(self
                .tint(FIND_CURRENT_TEXT)
                .unwrap_or(FIND_CURRENT_TEXT_NAMED))
            .bg(self.tint(fill).unwrap_or(FIND_CURRENT_NAMED))
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
            .remove_modifier(Modifier::REVERSED)
    }

    /// 按背景的明暗取 App 主题里的一种颜色，原样用（不柔化、不调对比度）。
    fn theme_color(&self, light: Rgb, dark: Rgb, named: Color) -> Style {
        let rgb = if self.light { light } else { dark };
        Style::default().fg(self.tint(rgb).unwrap_or(named))
    }

    /// 状态栏一类的颜色：深 / 浅背景各一种主题色，柔化后按背景调对比度；
    /// 16 色终端用 `named`。模型名与路径用它；别的 footer 统计要同一套规则时也用它。
    pub fn status_line(&self, dark: Rgb, light: Rgb, named: Color) -> Style {
        if self.level == ColorLevel::Ansi16 {
            return Style::default().fg(named);
        }
        let preferred = soften(if self.light { light } else { dark });
        let color = self
            .background
            .map_or(preferred, |background| readable(preferred, background));
        Style::default().fg(self.tint(color).unwrap_or(named))
    }
}

#[cfg(test)]
#[path = "tokens_tests.rs"]
mod tests;
