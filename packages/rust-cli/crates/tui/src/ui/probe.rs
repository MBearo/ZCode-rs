//! 终端的颜色事实（docs/specs/rust-cli/tui-kit.md §3.4）：颜色级别（终端变量与平台）与启动时探测到的前景、背景
//! （OSC 10 / 11）。墨色（[`super::ink::Ink`]）由它们与 `theme.json` 的选择解出；以前这里是旧的 `Palette`，派生色随
//! C2 删掉，只剩探测与级别。
use std::collections::BTreeMap;
use std::time::Duration;

pub type Rgb = (u8, u8, u8);

// 颜色探测的常量表（docs/specs/rust-cli/constants.md §5.3）。

/// Windows 10 起按版本号取颜色级别：build 14931 起真彩色，10586 起 256 色（tui.md §7.4）。
const WINDOWS_COLOR_MAJOR: u32 = 10;
const WINDOWS_TRUECOLOR_BUILD: u32 = 14931;
const WINDOWS_ANSI256_BUILD: u32 = 10586;
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

/// 终端的颜色事实：级别与探测到的前景、背景（不知道时 None）。runner 启动时得到一份；墨色由它解出。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colors {
    pub level: ColorLevel,
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
}

impl Colors {
    /// 只知道背景（或什么都不知道）的终端。
    pub fn new(level: ColorLevel, background: Option<Rgb>) -> Self {
        Self {
            level,
            foreground: None,
            background,
        }
    }

    /// 带上探测到的前景色（tui.md §7.4）。
    pub fn with_foreground(mut self, foreground: Option<Rgb>) -> Self {
        self.foreground = foreground;
        self
    }

    /// 缺省的主题选择（Terminal）下的墨色。
    pub fn ink(&self) -> super::ink::Ink {
        super::ink::Ink::new(self.level, self.foreground, self.background)
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

/// 探测到的终端颜色（tui.md §7.4）与终端支不支持程序状态（OSC 7501，tui-manage.md §5.4）。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Probed {
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub program_status: bool,
}

/// 从探测的回复里分别取 OSC 10（前景）与 OSC 11（背景）的颜色。只有 unix 的探测用它（Windows 读控制台，
/// `probe_console.rs`）；`ui` 改成 crate 内部后 Windows 上的未使用检查会报它（架构整理 U5）。
#[cfg(any(unix, test))]
pub fn parse_probe(reply: &str) -> Probed {
    let color = |code: &str| {
        let start = reply.find(&format!("\x1b]{code};"))?;
        parse_osc_color(&reply[start..])
    };
    Probed {
        foreground: color("10"),
        background: color("11"),
        program_status: super::program_status::supported(reply),
    }
}

/// 查询终端默认前景色与背景色（OSC 10、OSC 11，同一次探测查两者），后跟 DA1 作哨兵：所有终端都回应 DA1，收到它就
/// 不再等。最后回到行首擦掉不认识这些问题的终端打出来的字。同一次写里先问程序状态（OSC 7501，tui-manage.md §5.4）：终端按
/// 顺序回答，DA1 仍是最后一个。调用方已打开 raw mode，且 crossterm 还没开始读输入。
#[cfg(unix)]
pub fn probe_colors(timeout: Duration) -> Probed {
    probe_reply(timeout).map_or_else(Probed::default, |reply| parse_probe(&reply))
}

/// Unix 的查询：程序状态、前景、背景，DA1 作哨兵，再回到行首擦掉不认识的终端打出来的字。
#[cfg(any(unix, test))]
fn query() -> String {
    format!(
        "{}\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c\r\x1b[K",
        super::program_status::QUERY
    )
}

#[cfg(unix)]
fn probe_reply(timeout: Duration) -> Option<String> {
    use std::io::Write;
    use std::os::fd::AsFd;

    let mut out = std::io::stdout();
    out.write_all(query().as_bytes()).ok()?;
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
                program_status: false,
            }
        );
        assert_eq!(
            parse_probe("\x1b]11;rgb:ffff/ffff/ffff\x07\x1b[?62c"),
            Probed {
                foreground: None,
                background: Some((255, 255, 255)),
                program_status: false,
            }
        );
    }

    /// 程序状态（tui-manage.md §5.4）：同一次写里先问 7501，DA1 仍在最后；回复里有 7501 的回答才算支持。
    #[test]
    fn the_probe_also_asks_for_program_status() {
        assert_eq!(
            query(),
            "\x1b]7501;?\x1b\\\x1b]10;?\x1b\\\x1b]11;?\x1b\\\x1b[c\r\x1b[K"
        );
        let rex = "\x1b]7501;?:states=idle,working,done,blocked,error:kinds=permission,question,auth\x1b\\\
                   \x1b]11;rgb:0000/0000/0000\x1b\\\x1b[?62;22c";
        assert_eq!(
            parse_probe(rex),
            Probed {
                foreground: None,
                background: Some((0, 0, 0)),
                program_status: true,
            }
        );
        assert!(!parse_probe("\x1b]11;rgb:0000/0000/0000\x1b\\\x1b[?62c").program_status);
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
