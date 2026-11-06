//! 墨色的角色（docs/specs/rust-cli/tui-kit.md §3.4、§3.8，工作线 TK 的 C0）：旧界面 `Palette` / `tokens.rs` 的每个角色
//! （角色名照旧），改由 kit 的 [`Theme`] token 给出，返回 kit 的 [`Style`] / [`Color`]。C2 删掉了 `Palette` 与 `tokens.rs`，
//! 画面只经它取色；终端的颜色事实在 `probe.rs` 的 [`Colors`]。
//!
//! 取色照 Puffin 的壳给同一种画面用的 token（docs/puffin-tui.md §4.2）；Puffin 没有那种画面的，从 token 派生，标「自拟」。
//! 16 色（`theme.rich == false`）是 Puffin 的 plain 主题：fg2 / fg3 是 dim，强调色是 ANSI 色，没有底色。
//!
//! | 角色 | token / 派生 | Puffin 的出处 |
//! | --- | --- | --- |
//! | `secondary` | `fg3` | `shell/key_hints.rs` 的标签、`shell/picker.rs` 的 `current` 与提示行 |
//! | `key` | `fg2` | `shell/key_hints.rs` 的键 |
//! | `dim` | `fg3` | 三级墨色的「退后一步」（§4.2「墨色」） |
//! | `selection` | rich：`brand`；plain：`brand` 加粗 | `shell/kept.rs`、`settings/view.rs` 的 `chosen_style`（选中的行） |
//! | `accent` | `brand` | 标题、指针、选中的名字（`chosen_style`） |
//! | `model` | `fg2` | `shell/status_line.rs` 右端的模型名 |
//! | `path` | `fg3` | `shell/status_line.rs` 的 `~/目录` |
//! | `context_notice(low)` | 快满：`code`；否则 `fg2` | `shell/stats.rs` 的 `parts` / `near`（上下文的数变琥珀色） |
//! | `success` | `ok` | `shell/todo_dock.rs`、`goals.rs` 的 ✓ |
//! | `destructive` | `err` | `shell/failures.rs`、`shell_view.rs` 的失败 |
//! | `icon_blue`（自主模式） | `brand` | `shell/modes.rs` 的 `mode_style`：Autonomous 是 brand |
//! | `warning`（完全访问） | `code` | `shell/modes.rs` 的 `mode_style`：Full access 是 amber（`code`） |
//! | `bash` | `code` | `shell/shell_view.rs` 的 `!` 标记（`code` 粗体） |
//! | `prompt_block` / `prompt_bg` | `panel` 底 | `shell/view/frame.rs` 的 `pinned_row`、发出的消息的板 |
//! | `user_block` / `user_bg` | `panel` 底 | `shell/composer.rs` 输入框的板 |
//! | `mouse_selection` / `selection_bg` | rich：底色是 `line` 的前景色；plain：反显 | `shell/view/frame.rs` 的 `highlight` |
//! | `find_match` | 反显 | 自拟（Puffin 没有搜索；tui-kit.md §4.3） |
//! | `find_current` | rich：`code` 底、`bg` 字，粗体加下划线；plain：`code` 反显，粗体加下划线 | 自拟（tui-kit.md §4.3「`code` 底黑字」） |
//! | `hover_bg` | rich：`panel` 向 `line` 的前景色混合 40%；plain：没有 | 自拟（Puffin 没有悬停底色） |
//! | `subagent_ink` | `agent` | 自拟（`agent` 是 Puffin `@` 提及子代理的墨，`shell/at/rows.rs`） |
//! | `subagent_ground` | rich：`panel` 向 `agent` 混合 16%；plain：没有 | 自拟（tui-panels.md §1.6） |
//! | `tint(rgb)` | rich：原样的真彩；plain：没有 | 只给自带 RGB 的画面（logo 的金属色） |
//!
//! 颜色级别：rich 要求终端至少 256 色（`level != Ansi16`）并且报告了前景与背景（`Theme::resolve`）；256 色终端上的
//! 降采样由 kit 的渲染器做（tui-kit.md §3.4），这里只出真彩。
use rust_cli_tui_kit::render::{Color, Style, attr};
use rust_cli_tui_kit::theme::Rgb;
use rust_cli_tui_kit::theme::scheme::ThemeChoice;
use rust_cli_tui_kit::theme::theme::Theme;

use super::probe::{ColorLevel, Colors};

/// 悬停底色：`panel` 向 `line` 的前景色混合多少（自拟）。
const HOVER_TOWARD_LINE: f64 = 0.4;
/// 查看子代理时板的底色向 `agent` 混合的比例（tui-panels.md §1.6）。
const SUBAGENT_TOWARD_AGENT: f64 = 0.16;

/// 启动时探测到的终端事实与由它解出的主题；整个进程一份，渲染时只读（`/theme` 时重建）。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Ink {
    /// Puffin 的 token：画面只经它取色。
    pub theme: Theme,
    pub level: ColorLevel,
    /// 地是浅色：rich 时看主题的地，否则看探测到的背景。
    pub light: bool,
    /// 探测到的终端前景（OSC 10）。
    pub foreground: Option<Rgb>,
    /// 探测到的终端背景（OSC 11）。
    pub background: Option<Rgb>,
    /// `theme.json` 的选择（与 Puffin 同一个文件，tui-kit.md §3.4）：重建墨色（`/theme`、换背景）时沿用。
    pub choice: ThemeChoice,
}

fn kit_rgb((r, g, b): (u8, u8, u8)) -> Rgb {
    Rgb::new(r, g, b)
}

impl Ink {
    /// 缺省的主题选择（Terminal）下的墨色：探测到的颜色级别、前景与背景。
    pub fn new(
        level: ColorLevel,
        foreground: Option<(u8, u8, u8)>,
        background: Option<(u8, u8, u8)>,
    ) -> Self {
        Self::chosen(level, foreground, background, ThemeChoice::default())
    }

    /// `theme.json` 选的主题下的墨色（tui-kit.md §3.4）：runner 启动时读一次选择（`Services.stores`）。
    pub fn chosen(
        level: ColorLevel,
        foreground: Option<(u8, u8, u8)>,
        background: Option<(u8, u8, u8)>,
        choice: ThemeChoice,
    ) -> Self {
        let (foreground, background) = (foreground.map(kit_rgb), background.map(kit_rgb));
        let theme = Theme::resolve(
            foreground,
            background,
            level != ColorLevel::Ansi16,
            &choice,
            None,
        );
        let ground = theme.bg.or(background);
        Self {
            theme,
            level,
            light: ground.is_some_and(|bg| bg.lightness() >= 0.5),
            foreground,
            background,
            choice,
        }
    }

    /// 解出这份墨色的终端事实（`/theme` 换背景时按它重建）。
    pub fn colors(&self) -> Colors {
        let tuple = |c: Rgb| (c.r, c.g, c.b);
        Colors {
            level: self.level,
            foreground: self.foreground.map(tuple),
            background: self.background.map(tuple),
        }
    }

    /// 次要文字：说明、提示的标签、分隔。
    pub fn secondary(&self) -> Style {
        self.theme.fg3
    }

    /// 按键。
    pub fn key(&self) -> Style {
        self.theme.fg2
    }

    /// 退后一步的文字（旧的 SGR dim）。
    pub fn dim(&self) -> Style {
        self.theme.fg3
    }

    /// 选中的行（名字）：Puffin 的 `chosen_style`。
    pub fn selection(&self) -> Style {
        if self.theme.rich {
            Style::fg(self.theme.brand)
        } else {
            Style::new(self.theme.brand, Color::Default, attr::BOLD)
        }
    }

    /// 强调色：标题、指针、线。
    pub fn accent(&self) -> Color {
        self.theme.brand
    }

    pub fn model(&self) -> Style {
        self.theme.fg2
    }

    pub fn path(&self) -> Style {
        self.theme.fg3
    }

    /// footer 右侧的“上下文”提示：快满时琥珀色（Puffin 的 `near`），其余 fg2。
    pub fn context_notice(&self, low: bool) -> Style {
        if low {
            Style::fg(self.theme.code)
        } else {
            self.theme.fg2
        }
    }

    pub fn success(&self) -> Style {
        Style::fg(self.theme.ok)
    }

    pub fn destructive(&self) -> Style {
        Style::fg(self.theme.err)
    }

    /// 自主模式的名字。
    pub fn icon_blue(&self) -> Style {
        Style::fg(self.theme.brand)
    }

    /// 完全访问的名字。
    pub fn warning(&self) -> Style {
        Style::fg(self.theme.code)
    }

    /// bash 模式的提示符与边线。
    pub fn bash(&self) -> Color {
        self.theme.code
    }

    /// transcript 里用户消息的底色（板）；plain 没有。
    pub fn prompt_bg(&self) -> Option<Color> {
        self.theme.panel
    }

    /// 输入框的底色（板）；plain 没有。
    pub fn user_bg(&self) -> Option<Color> {
        self.theme.panel
    }

    /// 用户消息整块的样式。
    pub fn prompt_block(&self) -> Style {
        self.prompt_bg().map_or(Style::DEFAULT, Style::bg)
    }

    /// 输入框整块的样式。
    pub fn user_block(&self) -> Style {
        self.user_bg().map_or(Style::DEFAULT, Style::bg)
    }

    /// 悬停在可点击区域上的底色（自拟）；plain 没有（调用方改用下划线，与旧规则相同）。
    pub fn hover_bg(&self) -> Option<Color> {
        match (self.theme.panel, self.theme.line.fg_color()) {
            (Some(Color::TrueColor(r, g, b)), Color::TrueColor(lr, lg, lb)) => Some(
                Rgb::new(r, g, b)
                    .mix(Rgb::new(lr, lg, lb), HOVER_TOWARD_LINE)
                    .color(),
            ),
            _ => None,
        }
    }

    /// 鼠标选区的底色：rich 时是 `line` 的前景色；plain 没有（[`Self::mouse_selection`] 改用反显）。
    pub fn selection_bg(&self) -> Option<Color> {
        self.theme
            .rich
            .then(|| self.theme.line.fg_color())
            .filter(|c| !c.is_default())
    }

    /// 鼠标选区盖住的格子的样式：Puffin `view/frame.rs` 的 `highlight` 的 `mark`。
    pub fn mouse_selection(&self, style: Style) -> Style {
        match self.selection_bg() {
            Some(bg) => style.with_bg(bg),
            None => style.with_attr(attr::REVERSE),
        }
    }

    /// 搜索的匹配：反显。
    pub fn find_match(&self) -> Style {
        Style::REVERSE
    }

    /// 搜索模式的当前匹配：琥珀底、地色的字，粗体加下划线。**替换**匹配的样式，不叠在 [`Self::find_match`] 上
    /// （kit 的样式没有“去掉某一位”）。
    pub fn find_current(&self) -> Style {
        let marks = attr::BOLD | attr::UNDERLINE;
        match self.theme.bg {
            Some(ground) if self.theme.rich => Style::new(ground.color(), self.theme.code, marks),
            _ => Style::new(self.theme.code, Color::Default, marks | attr::REVERSE),
        }
    }

    /// 查看子代理时标题行的描述与 plain 的板的两条线。
    pub fn subagent_ink(&self) -> Style {
        Style::fg(self.theme.agent)
    }

    /// 查看子代理时板的底色：`panel` 向 `agent` 混一点；plain 没有。
    pub fn subagent_ground(&self) -> Option<Color> {
        match (self.theme.panel, self.theme.agent) {
            (Some(Color::TrueColor(r, g, b)), Color::TrueColor(ar, ag, ab)) => Some(
                Rgb::new(r, g, b)
                    .mix(Rgb::new(ar, ag, ab), SUBAGENT_TOWARD_AGENT)
                    .color(),
            ),
            _ => None,
        }
    }

    /// 一个自带的 RGB（logo 的金属色）按主题画：rich 原样，plain 没有。
    pub fn tint(&self, rgb: (u8, u8, u8)) -> Option<Color> {
        self.theme.rich.then(|| kit_rgb(rgb).color())
    }
}

#[cfg(test)]
#[path = "ink_tests.rs"]
mod tests;
