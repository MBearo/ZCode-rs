// 复制自 apps/zcode-host/crates/tui/src/ui/palette.rs（feat/rs 79b6691c55）。
//! 配色（docs/specs/rust-tui.md §8.6）：以终端自身调色板为主。启动时查询终端背景色，
//! 用户消息与输入框的底色、强调色都由它派生；查不到时按深色背景处理。
use std::collections::BTreeMap;
use std::time::Duration;

use ratatui::style::{Color, Modifier, Style};

pub type Rgb = (u8, u8, u8);

// 调色板的常量表（docs/specs/rust-cli/constants.md §5.3）。

/// 纯白与纯黑。
pub const WHITE: Rgb = (255, 255, 255);
pub const BLACK: Rgb = (0, 0, 0);
/// Rec. 601 的亮度权重（红、绿、蓝）；加权和大于 128 算浅色。
pub const LUMA_WEIGHTS: [f64; 3] = [0.299, 0.587, 0.114];
const LIGHT_LUMA_ABOVE: f64 = 128.0;
/// Windows 10 起按版本号取颜色级别：build 14931 起真彩色，10586 起 256 色（tui.md §7.4）。
const WINDOWS_COLOR_MAJOR: u32 = 10;
const WINDOWS_TRUECOLOR_BUILD: u32 = 14931;
const WINDOWS_ANSI256_BUILD: u32 = 10586;
/// 背景色上叠白（深色背景）或叠黑（浅色背景）的比例：（深色, 浅色）。
const USER_BG_TINT: (f64, f64) = (0.12, 0.04);
const PROMPT_BG_TINT: (f64, f64) = (0.16, 0.02);
const HOVER_BG_TINT: (f64, f64) = (0.20, 0.07);
/// 鼠标选区的底色（主题的 `selectionBg`）。
const SELECTION_BG_DARK: Rgb = (38, 79, 120);
const SELECTION_BG_LIGHT: Rgb = (180, 213, 255);
/// bash 模式的提示符与边线（`bashBorder`）。
const BASH_DARK: Rgb = (253, 93, 177);
const BASH_LIGHT: Rgb = (255, 0, 135);
/// xterm 256 色：6×6×6 色块从 16 号起，每个分量取这 6 级；24 级灰度从 232 号起，从 8 起每级加 10。
const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
const CUBE_START: u8 = 16;
const CUBE_SIDE: u8 = CUBE_LEVELS.len() as u8;
const GRAY_START: u8 = 232;
const GRAY_STEPS: u8 = 24;
const GRAY_FIRST_LEVEL: u32 = 8;
const GRAY_STEP: u32 = 10;
const GRAY_LAST_LEVEL: u32 = GRAY_FIRST_LEVEL + GRAY_STEP * (GRAY_STEPS as u32 - 1);
/// 一个颜色的分量数（红、绿、蓝）。
const CHANNELS: u32 = 3;
/// OSC 颜色回复的每段是十六进制，每位 4 比特。
const HEX_RADIX: u32 = 16;
const BITS_PER_HEX_DIGIT: u32 = 4;

/// 终端能显示的颜色数。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorLevel {
    TrueColor,
    Ansi256,
    Ansi16,
}

impl ColorLevel {
    /// 终端变量与平台（入口注入）给出的颜色级别（docs/specs/rust-cli/tui.md §7.4）。
    pub fn detect(vars: &BTreeMap<String, String>, os: crate::os::Os) -> Self {
        // 缺陷修复（F65 的 T12）：Windows Terminal 与 conhost 都不设 `COLORTERM` / `TERM`，以前一律按 16 色画。Windows
        // 上按系统版本取（Windows 10 build 14931 起真彩色，10586 起 256 色），不看这两个变量。
        if let crate::os::Os::Windows(release) = os {
            return match release {
                Some(r) if r.major >= WINDOWS_COLOR_MAJOR && r.build >= WINDOWS_TRUECOLOR_BUILD => {
                    Self::TrueColor
                }
                Some(r) if r.major >= WINDOWS_COLOR_MAJOR && r.build >= WINDOWS_ANSI256_BUILD => {
                    Self::Ansi256
                }
                _ => Self::Ansi16,
            };
        }
        let colorterm = vars
            .get(rust_cli_base::names::env::COLORTERM)
            .map(|v| v.to_ascii_lowercase());
        if colorterm.is_some_and(|v| v == "truecolor" || v == "24bit") {
            return Self::TrueColor;
        }
        let term = vars
            .get(rust_cli_base::names::env::TERM)
            .map(String::as_str)
            .unwrap_or_default();
        if term.contains("256") || term.contains("direct") {
            return Self::Ansi256;
        }
        // macOS 自带的 Terminal 声明 xterm-256color；其他终端缺省给 16 色，画面仍可读。
        Self::Ansi16
    }
}

/// 启动时得到的终端事实与派生色。整个进程只有一份，渲染时只读。
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    pub level: ColorLevel,
    pub background: Option<Rgb>,
    /// 终端的前景色（OSC 10，修复线 F26）；与背景的明暗相同时（`/theme` 换了背景）不用。次要文字与按键提示的颜色
    /// 要前景与背景两个都知道（docs/specs/rust-cli/tui.md §7.4，Codex 的规则）。
    pub foreground: Option<Rgb>,
    pub light: bool,
    /// 输入框的底色；16 色终端没有底色。
    pub user_bg: Option<Color>,
    /// transcript 里用户消息的底色（比输入框深一档）。
    pub prompt_bg: Option<Color>,
    /// 鼠标悬停在可点击区域上时的底色（§8.7）；16 色终端为 None（改用下划线）。
    pub hover_bg: Option<Color>,
    pub accent: Color,
    /// 鼠标选区的底色（主题的 `selectionBg`，tui-select.md §2.2）：深色 `rgb(38,79,120)`、浅色 `rgb(180,213,255)`；
    /// 16 色终端深色用蓝、浅色用青（ansi 主题）。
    pub selection_bg: Color,
}

pub fn is_light((r, g, b): Rgb) -> bool {
    // 与 Rec. 601 亮度近似：大于一半即浅色背景。
    luma((r, g, b)) > LIGHT_LUMA_ABOVE
}

/// Rec. 601 的亮度（0–255）。
pub fn luma((r, g, b): Rgb) -> f64 {
    let [wr, wg, wb] = LUMA_WEIGHTS;
    wr * f64::from(r) + wg * f64::from(g) + wb * f64::from(b)
}

pub fn blend(base: Rgb, top: Rgb, alpha: f64) -> Rgb {
    let mix = |a: u8, b: u8| (f64::from(a) * (1.0 - alpha) + f64::from(b) * alpha).round() as u8;
    (mix(base.0, top.0), mix(base.1, top.1), mix(base.2, top.2))
}

fn to_color(rgb: Rgb, level: ColorLevel) -> Option<Color> {
    match level {
        ColorLevel::TrueColor => Some(Color::Rgb(rgb.0, rgb.1, rgb.2)),
        ColorLevel::Ansi256 => Some(Color::Indexed(nearest_256(rgb))),
        ColorLevel::Ansi16 => None,
    }
}

/// xterm 256 色里与 `rgb` 最近的一格（6×6×6 色块与 24 级灰度）。
pub fn nearest_256((r, g, b): Rgb) -> u8 {
    let steps = CUBE_LEVELS;
    let index = |v: u8| {
        steps
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| (i32::from(**s) - i32::from(v)).abs())
            .map_or(0, |(i, _)| i as u8)
    };
    let (ri, gi, bi) = (index(r), index(g), index(b));
    let cube = CUBE_START + CUBE_SIDE * CUBE_SIDE * ri + CUBE_SIDE * gi + bi;
    let cube_rgb = (steps[ri as usize], steps[gi as usize], steps[bi as usize]);
    let gray_level = ((u32::from(r) + u32::from(g) + u32::from(b)) / CHANNELS).min(GRAY_LAST_LEVEL);
    let gray_index = (gray_level.saturating_sub(GRAY_FIRST_LEVEL) / GRAY_STEP)
        .min(u32::from(GRAY_STEPS) - 1) as u8;
    let gray = (GRAY_FIRST_LEVEL + GRAY_STEP * u32::from(gray_index)) as u8;
    // 缺陷修复（修复线 F26）：原来参数也叫 `b`，遮住了目标颜色的蓝色分量，比较的是候选色自己的蓝与绿，偏蓝的颜色
    // 会落到灰阶上（(99,168,248) 成了灰色 248）。
    let distance = |(cr, cg, cb): Rgb| {
        let d = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2);
        d(cr, r) + d(cg, g) + d(cb, b)
    };
    if distance((gray, gray, gray)) < distance(cube_rgb) {
        GRAY_START + gray_index
    } else {
        cube
    }
}

impl Palette {
    pub fn new(level: ColorLevel, background: Option<Rgb>) -> Self {
        let light = background.is_some_and(is_light);
        let tint = |(dark, bright): (f64, f64)| {
            background.and_then(|bg| {
                let mixed = if light {
                    blend(bg, BLACK, bright)
                } else {
                    blend(bg, WHITE, dark)
                };
                to_color(mixed, level)
            })
        };
        let user_bg = tint(USER_BG_TINT);
        let prompt_bg = tint(PROMPT_BG_TINT);
        let hover_bg = tint(HOVER_BG_TINT);
        // 强调色取 `tokens` 的命名常量（F26，取值不变：真彩色 (99,168,248) / (28,100,200)，256 色 75 / 26，16 色青色）。
        use super::tokens::{ACCENT_DARK, ACCENT_LIGHT, ACCENT_NAMED};
        let accent =
            to_color(if light { ACCENT_LIGHT } else { ACCENT_DARK }, level).unwrap_or(ACCENT_NAMED);
        Self {
            level,
            background,
            foreground: None,
            light,
            user_bg,
            prompt_bg,
            hover_bg,
            accent,
            selection_bg: {
                let rgb = if light {
                    SELECTION_BG_LIGHT
                } else {
                    SELECTION_BG_DARK
                };
                to_color(rgb, level).unwrap_or(if light { Color::Cyan } else { Color::Blue })
            },
        }
    }

    /// 带上探测到的前景色（tui.md §7.4）。
    pub fn with_foreground(mut self, foreground: Option<Rgb>) -> Self {
        self.foreground = foreground;
        self
    }

    /// bash 模式的提示符与边线（`bashBorder`：深色 `rgb(253,93,177)`、浅色 `rgb(255,0,135)`，
    /// 16 色终端是品红，tui-input.md §2.1）。
    pub fn bash(&self) -> Color {
        let rgb = if self.light { BASH_LIGHT } else { BASH_DARK };
        to_color(rgb, self.level).unwrap_or(if self.light {
            Color::Magenta
        } else {
            Color::LightMagenta
        })
    }

    /// 一个固定的 RGB 颜色按终端色深转换；16 色终端返回 None（不画底色）。
    pub fn tint(&self, rgb: Rgb) -> Option<Color> {
        to_color(rgb, self.level)
    }

    /// transcript 里用户消息整块的样式。
    pub fn prompt_block(&self) -> Style {
        self.prompt_bg
            .map_or_else(Style::default, |bg| Style::default().bg(bg))
    }

    /// 输入框整块的样式。
    pub fn user_block(&self) -> Style {
        self.user_bg
            .map_or_else(Style::default, |bg| Style::default().bg(bg))
    }

    pub fn dim(&self) -> Style {
        Style::default().add_modifier(Modifier::DIM)
    }
}

/// 从 OSC 11 回复里取 `rgb:RRRR/GGGG/BBBB`（每段 1–4 位十六进制）。
pub fn parse_osc_color(reply: &str) -> Option<Rgb> {
    let rgb = reply.split("rgb:").nth(1)?;
    let mut parts = rgb.split('/');
    let mut channel = || -> Option<u8> {
        let hex: String = parts
            .next()?
            .chars()
            .take_while(char::is_ascii_hexdigit)
            .collect();
        let value = u32::from_str_radix(&hex, HEX_RADIX).ok()?;
        let max = (1u32 << (BITS_PER_HEX_DIGIT * hex.len() as u32)) - 1;
        Some((value * u32::from(u8::MAX) / max.max(1)) as u8)
    };
    Some((channel()?, channel()?, channel()?))
}

/// 探测到的终端颜色（tui.md §7.4）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Probed {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
}

/// 从探测的回复里分别取 OSC 10（前景）与 OSC 11（背景）的颜色。
pub fn parse_probe(reply: &str) -> Probed {
    let color = |code: &str| {
        let start = reply.find(&format!("\x1b]{code};"))?;
        parse_osc_color(&reply[start..])
    };
    Probed {
        foreground: color("10"),
        background: color("11"),
    }
}

/// 查询终端默认前景色与背景色（OSC 10、OSC 11；Codex 同一次探测查两者），后跟 DA1 作哨兵：所有终端都回应
/// DA1，收到它就不再等。调用方已打开 raw mode，且 crossterm 还没开始读输入。
#[cfg(unix)]
pub fn probe_colors(timeout: Duration) -> Probed {
    probe_reply(timeout).map_or_else(Probed::default, |reply| parse_probe(&reply))
}

#[cfg(unix)]
fn probe_reply(timeout: Duration) -> Option<String> {
    use std::io::Write;
    use std::os::fd::AsFd;

    let mut out = std::io::stdout();
    out.write_all(b"\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c")
        .ok()?;
    out.flush().ok()?;
    let stdin = std::io::stdin();
    let fd = stdin.as_fd();
    let deadline = std::time::Instant::now() + timeout;
    let mut reply = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(std::time::Instant::now());
        if left.is_zero() {
            break;
        }
        let wait = rustix::event::Timespec {
            tv_sec: left.as_secs() as _,
            tv_nsec: left.subsec_nanos() as _,
        };
        let mut fds = [rustix::event::PollFd::new(
            &fd,
            rustix::event::PollFlags::IN,
        )];
        if !matches!(rustix::event::poll(&mut fds, Some(&wait)), Ok(n) if n > 0) {
            break;
        }
        let mut chunk = [0u8; 256];
        let Ok(read) = rustix::io::read(fd, &mut chunk) else {
            break;
        };
        if read == 0 {
            break;
        }
        reply.extend_from_slice(&chunk[..read]);
        // DA1 回复以 `ESC [ ?` 开头、`c` 结尾。
        let text = String::from_utf8_lossy(&reply);
        if let Some(start) = text.find("\x1b[?")
            && text[start..].contains('c')
        {
            break;
        }
    }
    Some(String::from_utf8_lossy(&reply).into_owned())
}

/// Windows（工作线 F71，tui.md §7.4 的 T13）：同样查 OSC 10、OSC 11，回复从控制台的输入记录里读（`probe_console`）。
/// 以前 Windows 不探测，浅色主题的终端也按深色配色。
#[cfg(windows)]
pub fn probe_colors(timeout: Duration) -> Probed {
    super::probe_console::probe(timeout)
}

#[cfg(not(any(unix, windows)))]
pub fn probe_colors(_timeout: Duration) -> Probed {
    Probed::default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc_replies_of_any_width_are_parsed() {
        assert_eq!(
            parse_osc_color("\x1b]11;rgb:ffff/0000/8080\x1b\\"),
            Some((255, 0, 128))
        );
        assert_eq!(
            parse_osc_color("\x1b]11;rgb:1e/1e/2e\x07"),
            Some((30, 30, 46))
        );
        assert_eq!(parse_osc_color("\x1b[?62c"), None);
        // 修复线 F26：前景（OSC 10）与背景（OSC 11）分开取；只回了背景时前景为 None。
        assert_eq!(
            parse_probe(
                "\x1b]10;rgb:cdcd/d6d6/f4f4\x1b\\\x1b]11;rgb:1e1e/1e1e/2e2e\x1b\\\x1b[?62c"
            ),
            Probed {
                foreground: Some((205, 214, 244)),
                background: Some((30, 30, 46)),
            }
        );
        assert_eq!(
            parse_probe("\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[?62c"),
            Probed {
                foreground: None,
                background: Some((255, 255, 255)),
            }
        );
    }

    #[test]
    fn derived_colors_follow_the_background_and_the_color_level() {
        let dark = Palette::new(ColorLevel::TrueColor, Some((0, 0, 0)));
        assert!(!dark.light);
        assert_eq!(dark.user_bg, Some(Color::Rgb(31, 31, 31)));
        let light = Palette::new(ColorLevel::TrueColor, Some((255, 255, 255)));
        assert!(light.light);
        assert_eq!(light.user_bg, Some(Color::Rgb(245, 245, 245)));
        assert!(
            Palette::new(ColorLevel::Ansi16, Some((0, 0, 0)))
                .user_bg
                .is_none()
        );
        assert!(matches!(
            Palette::new(ColorLevel::Ansi256, Some((0, 0, 0))).user_bg,
            Some(Color::Indexed(_))
        ));
        // 查不到背景：按深色处理，没有底色。
        let unknown = Palette::new(ColorLevel::TrueColor, None);
        assert!(!unknown.light && unknown.user_bg.is_none());
    }

    #[test]
    fn nearest_256_prefers_grays_for_grays() {
        assert_eq!(nearest_256((31, 31, 31)), 234);
        assert_eq!(nearest_256((255, 0, 0)), 196);
        // 修复线 F26：偏蓝的颜色取色块，不落到灰阶上（原来是 248）。
        assert_eq!(nearest_256((99, 168, 248)), 75);
        // 强调色改从命名常量取之后，256 色的取值与原来写死的相同。
        assert_eq!(
            Palette::new(ColorLevel::Ansi256, Some((0, 0, 0))).accent,
            Color::Indexed(75)
        );
        assert_eq!(
            Palette::new(ColorLevel::Ansi256, Some((255, 255, 255))).accent,
            Color::Indexed(26)
        );
    }

    #[test]
    fn the_color_level_comes_from_the_environment() {
        let vars = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| ((*k).into(), (*v).into()))
                .collect()
        };
        let unix = crate::os::Os::Unix;
        assert_eq!(
            ColorLevel::detect(
                &vars(&[(rust_cli_base::names::env::COLORTERM, "truecolor")]),
                unix
            ),
            ColorLevel::TrueColor
        );
        assert_eq!(
            ColorLevel::detect(
                &vars(&[(rust_cli_base::names::env::TERM, "xterm-256color")]),
                unix
            ),
            ColorLevel::Ansi256
        );
        assert_eq!(
            ColorLevel::detect(&vars(&[(rust_cli_base::names::env::TERM, "xterm")]), unix),
            ColorLevel::Ansi16
        );
    }

    /// 修复线 F65 的 T12：Windows 上按系统版本（入口经 `RtlGetVersion` 读）取，不看 `COLORTERM` / `TERM`（Windows
    /// Terminal 与 conhost 都不设它们，以前一律 16 色）。
    #[test]
    fn windows_takes_the_color_level_from_the_build() {
        use crate::os::{Os, WindowsRelease};
        let windows = |major, build| Os::Windows(Some(WindowsRelease { major, build }));
        let none = BTreeMap::new();
        assert_eq!(
            ColorLevel::detect(&none, windows(10, 22631)),
            ColorLevel::TrueColor
        );
        assert_eq!(
            ColorLevel::detect(&none, windows(10, 14931)),
            ColorLevel::TrueColor
        );
        assert_eq!(
            ColorLevel::detect(&none, windows(10, 14930)),
            ColorLevel::Ansi256
        );
        assert_eq!(
            ColorLevel::detect(&none, windows(10, 10586)),
            ColorLevel::Ansi256
        );
        assert_eq!(
            ColorLevel::detect(&none, windows(10, 10240)),
            ColorLevel::Ansi16
        );
        assert_eq!(
            ColorLevel::detect(&none, windows(6, 9600)),
            ColorLevel::Ansi16
        );
        assert_eq!(
            ColorLevel::detect(&none, Os::Windows(None)),
            ColorLevel::Ansi16
        );
        let mut colorterm = BTreeMap::new();
        colorterm.insert(
            rust_cli_base::names::env::COLORTERM.to_owned(),
            "truecolor".to_owned(),
        );
        assert_eq!(
            ColorLevel::detect(&colorterm, windows(10, 10586)),
            ColorLevel::Ansi256
        );
    }
}
