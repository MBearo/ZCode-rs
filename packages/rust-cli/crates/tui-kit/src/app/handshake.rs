//! 启动时与终端的字节流协商（docs/puffin-tui.md §6 规则 3、§4.2「主题」、§4.3「按键解码」）。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Handshake.scala（feat/tui a2bf878b34）；kitty 键盘级别的选择移植自
//! packages/tui/tui/src/zcode/tui/platform/NodeTerminal.scala 的 `keyboardEnhancement`。
//!
//! 这里是纯的：不读写终端、不起定时器、不碰进程级的宽度。运行时这样驱动一个 [`Handshake`]：
//!
//! 1. 进入原始模式、接好输入字节的路由之后，写出 [`Handshake::request`]，再画首帧（首帧不等握手：先按缺省宽度画）。
//!    同时起一个 [`TIMEOUT`] 的定时器。
//! 2. 握手进行时，每个输入字节先经 [`Handshake::feed`]：`Step::passthrough` 的字节按顺序交给 `KeyParser`；
//!    `Step::done` 时落定。
//! 3. 落定（`done` 或定时器到，先到的那个；之后的那个什么也不做）：调用一次 [`Handshake::settle`]，然后
//!    - 若 `Settled::widths` 是 `Some`，记下 `width::generation()`，调用 [`Measured::adopt`]；代数变了就整屏重画
//!      （`Cmd::Refresh`），没变就按格子差异重画。
//!    - 把 `Settled::replay` 的字节按顺序交给 `KeyParser`（与终端回答赛跑的按键，一个不丢）。
//!    - [`TerminalProbe`] 的报告在 `settle` 里经构造时给的回调交出（回调把它变成消息放进事件队列）；重画排在那条
//!      消息之后，用上探测的结果。
//! 4. 此后输入字节直接进 `KeyParser`。
use std::collections::BTreeMap;
use std::time::Duration;

use rust_cli_base::names::env;

use crate::render::ansi;
use crate::render::width::{self, Widths};
use crate::theme::Rgb;

/// 终端多久不回答就按没有回答处理（docs/puffin-tui.md §6）。无头终端从不回答，只会走这个超时。
pub const TIMEOUT: Duration = Duration::from_millis(300);

/// 消费一个输入字节的结果。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Step {
    /// 认出来不属于握手的字节（与回答赛跑的按键），按到达顺序。
    pub passthrough: Vec<u8>,
    /// 握手完成了。
    pub done: bool,
}

impl Step {
    /// 字节被握手收下，还没完成。
    pub const CONSUMED: Step = Step {
        passthrough: Vec::new(),
        done: false,
    };

    fn leak(passthrough: Vec<u8>) -> Step {
        Step {
            passthrough,
            done: false,
        }
    }

    const fn done() -> Step {
        Step {
            passthrough: Vec::new(),
            done: true,
        }
    }
}

/// 落定的结果：运行时要做的事，作为数据交出。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Settled {
    /// 还扣着的字节（超时时不完整的回答，或与回答赛跑的按键），按顺序交给 `KeyParser`。
    pub replay: Vec<u8>,
    /// 量到并且可信的类宽度；`None` 是保持缺省（没量完，或 CJK 的检验字形量错了）。
    pub widths: Option<Measured>,
}

/// 启动握手。
pub trait Handshake: Send {
    /// 进入原始模式后写给终端的字节。
    fn request(&self) -> String;

    /// 消费一个输入字节。
    fn feed(&mut self, b: u8) -> Step;

    /// 结束握手：不论 [`Handshake::feed`] 报了完成还是运行时超时，只调用一次（多调用也安全）。
    fn settle(&mut self) -> Settled;
}

/// 光标位置探测量到的三类宽度（终端自己的意见），由运行时采纳。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Measured {
    pub emoji_bmp: u32,
    pub emoji_astral: u32,
    pub ambiguous: u32,
}

impl Measured {
    /// 采纳后的宽度：纯值，测试与运行时都可以用。不合理的值（1、2 以外）按类忽略。
    pub fn apply(self, widths: Widths) -> Widths {
        widths.adopt(self.emoji_bmp, self.emoji_astral, self.ambiguous)
    }

    /// 进程级地采纳（[`width::adopt`]）。只由运行时在落定时调用。
    pub fn adopt(self) {
        width::adopt(self.emoji_bmp, self.emoji_astral, self.ambiguous);
    }
}

/// 探测的字形：✅（BMP emoji 类）、😀（辅助平面 emoji 类）、①（EAW 模糊类）、一（通用的 CJK——检验字形）。
const GLYPHS: [char; 4] = ['\u{2705}', '\u{1f600}', '\u{2460}', '\u{4e00}'];

/// 量终端自己对有争议的宽度类的意见。
///
/// [`Handshake::request`] 把每个探测字形打在第 0 列，再问一次光标位置（`CSI 6n`）；回答的列减一就是终端给这个
/// 字形的宽度——不管它按哪版 Unicode 表编译，这是唯一算数的权威。探测的那一行在同一次写里擦掉（整个请求包在
/// DEC 2026 同步里，支持的终端连闪都不闪）。落定时以 CJK 检验字形为门：一在任何正常的 UTF-8 终端里都量出 2，否则
/// 回答不可信（非 UTF-8 的链路出的乱码、过滤的复用器），保持现代终端的缺省。
///
/// CPR 回答（`ESC [ row ; col R`）由一个小状态机认出；与这个形状分叉的字节——与回答赛跑的按键、鼠标报告——原样交给
/// 按键解析。DSR 固有的歧义（修饰过的 F3 也是 `CSI 1;…R`）只限于几毫秒的探测窗口。
#[derive(Debug, Default)]
pub struct WidthProbe {
    /// 每个回答量出的宽度（列减一；没有列时是 -2）。
    widths: Vec<i64>,
    pending: Vec<u8>,
    state: CprState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum CprState {
    #[default]
    Idle,
    AfterEsc,
    Params,
}

impl WidthProbe {
    pub fn new() -> WidthProbe {
        WidthProbe::default()
    }

    /// [`Handshake::request`] 里字形与 CPR 的部分，不带同步包装与擦除，[`TerminalProbe`] 好在同一个包装里多问几样。
    pub fn queries() -> String {
        let mut s = String::new();
        for glyph in GLYPHS {
            s.push_str(ansi::CARRIAGE_RETURN);
            s.push(glyph);
            s.push_str(ansi::CSI);
            s.push_str("6n");
        }
        s
    }

    /// 不是 CPR：放出扣着的全部字节加上这个，按顺序。
    fn divergence(&mut self, b: u8) -> Step {
        let mut leak = std::mem::take(&mut self.pending);
        leak.push(b);
        self.state = CprState::Idle;
        Step::leak(leak)
    }

    /// `ESC [ row ; col R` 的列：最后一个 `;` 之后的数字。没有列或不是数字时是 -1。
    fn column(&self) -> i64 {
        let Some(semi) = self.pending.iter().rposition(|&c| c == b';') else {
            return -1;
        };
        if semi < 2 || semi + 1 >= self.pending.len() {
            return -1;
        }
        let digits = &self.pending[semi + 1..];
        if !digits.iter().all(u8::is_ascii_digit) {
            return -1;
        }
        digits.iter().fold(0i64, |v, &d| {
            v.saturating_mul(10).saturating_add(i64::from(d - b'0'))
        })
    }

    /// 量完四个字形且检验字形是 2 时的宽度。
    fn measured(&self) -> Option<Measured> {
        if self.widths.len() != GLYPHS.len() || self.widths[3] != 2 {
            return None;
        }
        // 负数（没有列）与过大的值都不是 1 或 2，采纳时按类忽略；这里折成 0 一样被忽略。
        let class = |i: usize| u32::try_from(self.widths[i]).unwrap_or(0);
        Some(Measured {
            emoji_bmp: class(0),
            emoji_astral: class(1),
            ambiguous: class(2),
        })
    }
}

impl Handshake for WidthProbe {
    fn request(&self) -> String {
        let mut s = String::from(ansi::SYNC_BEGIN);
        s.push_str(&WidthProbe::queries());
        s.push_str(ansi::CARRIAGE_RETURN);
        s.push_str(ansi::ERASE_TO_EOL);
        s.push_str(ansi::SYNC_END);
        s
    }

    fn feed(&mut self, b: u8) -> Step {
        match self.state {
            CprState::Idle => {
                if b == 0x1b {
                    self.pending.push(b);
                    self.state = CprState::AfterEsc;
                    Step::CONSUMED
                } else {
                    Step::leak(vec![b])
                }
            }
            CprState::AfterEsc => {
                if b == b'[' {
                    self.pending.push(b);
                    self.state = CprState::Params;
                    Step::CONSUMED
                } else {
                    self.divergence(b)
                }
            }
            CprState::Params => {
                if b.is_ascii_digit() || b == b';' {
                    self.pending.push(b);
                    Step::CONSUMED
                } else if b == b'R' {
                    // 列是打在第 0 列的字形之后的光标，所以字形的宽度是列减一。
                    let col = self.column();
                    self.pending.clear();
                    self.state = CprState::Idle;
                    self.widths.push(col - 1);
                    Step {
                        passthrough: Vec::new(),
                        done: self.widths.len() >= GLYPHS.len(),
                    }
                } else {
                    self.divergence(b)
                }
            }
        }
    }

    fn settle(&mut self) -> Settled {
        Settled {
            replay: std::mem::take(&mut self.pending),
            widths: self.measured(),
        }
    }
}

/// 单元格的像素尺寸，来自 `CSI 16 t` 的回答 `CSI 6 ; height ; width t`。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellPx {
    pub width: u32,
    pub height: u32,
}

/// 终端告诉 [`TerminalProbe`] 的东西。每个字段都是真回答了的；没回答的问题留缺省。
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TerminalReport {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    pub kitty_graphics: bool,
    pub sixel: bool,
    pub cell_px: Option<CellPx>,
}

/// [`TerminalProbe`] 落定时交出报告的回调。
pub type OnSettled = Box<dyn FnOnce(TerminalReport) + Send>;

/// 启动探测（docs/puffin-tui.md §6 规则 3）。一个握手依次问：字形宽度（[`WidthProbe`]）、前景与背景色
/// （OSC 10/11）、kitty 图形是否可用（`a=q` 查询）、单元格的像素尺寸（`CSI 16 t`），最后是 DA1（`CSI c`）。终端按
/// 顺序回答，每个终端都回答 DA1，所以它的回答结束探测；它的参数还说明是否支持 sixel（`4`）。不懂某个问题的终端要么
/// 忽略，要么把它当文字打出来，结尾的擦行把它擦掉。
///
/// 回答切成完整的转义序列（CSI、OSC、APC）。不是预期回答的序列——方向键、Alt-x——与任何普通字节按到达顺序交给按键
/// 解析。
///
/// 落定时交出宽度（[`Settled::widths`]），并把报告交给 `on_settled`，只交一次。
pub struct TerminalProbe {
    widths: WidthProbe,
    pending: Vec<u8>,
    state: ReplyState,
    report: TerminalReport,
    on_settled: Option<OnSettled>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum ReplyState {
    #[default]
    Idle,
    AfterEsc,
    Csi,
    Osc,
    OscAfterEsc,
    Apc,
    ApcAfterEsc,
}

/// 问 kitty 图形能不能用：一张 1×1 的 RGB 图，只查询（`a=q`），id 31。
pub const KITTY_QUERY: &str = "\x1b_Gi=31,s=1,v=1,a=q,t=d,f=24;AAAA\x1b\\";

impl TerminalProbe {
    pub fn new(on_settled: impl FnOnce(TerminalReport) + Send + 'static) -> TerminalProbe {
        TerminalProbe {
            widths: WidthProbe::new(),
            pending: Vec::new(),
            state: ReplyState::Idle,
            report: TerminalReport::default(),
            on_settled: Some(Box::new(on_settled)),
        }
    }

    /// 到目前为止收到的报告。
    pub fn report(&self) -> TerminalReport {
        self.report
    }

    /// 不是回答：放出扣着的全部字节加上 `b`，按顺序。
    fn release(&mut self, b: u8) -> Step {
        let mut leak = std::mem::take(&mut self.pending);
        leak.push(b);
        self.state = ReplyState::Idle;
        Step::leak(leak)
    }

    /// `pending[2..]` 作为文字（回答里只有 ASCII；别的字节按 Latin-1，与 Scala 的 `toChar` 相同）。
    fn body(&self) -> String {
        self.pending[2..].iter().map(|&c| char::from(c)).collect()
    }

    /// `pending` 里是一个完整的 CSI 序列（ESC [ 参数 结尾）。
    fn csi(&mut self) -> Step {
        let bytes = std::mem::take(&mut self.pending);
        self.state = ReplyState::Idle;
        let fin = bytes[bytes.len() - 1];
        let params: String = bytes[2..bytes.len() - 1]
            .iter()
            .map(|&c| char::from(c))
            .collect();
        match fin {
            b'R' => {
                // 光标位置报告：一个宽度样本。
                for b in bytes {
                    self.widths.feed(b);
                }
                Step::CONSUMED
            }
            b'c' if params.starts_with('?') => {
                self.report.sixel = params[1..].split(';').any(|p| p == "4");
                Step::done()
            }
            b't' if params.starts_with("6;") => {
                if let [_, h, w] = split_trimmed(&params, ';').as_slice()
                    && let (Ok(h), Ok(w)) = (h.parse::<i32>(), w.parse::<i32>())
                    && h > 0
                    && w > 0
                {
                    self.report.cell_px = Some(CellPx {
                        width: w as u32,
                        height: h as u32,
                    });
                }
                Step::CONSUMED
            }
            // 探测进行时打的键（方向键、功能键）。
            _ => Step::leak(bytes),
        }
    }

    /// 一个 OSC 回答的主体（不含引导与结尾）。
    fn osc(&mut self, body: &str) -> Step {
        match body.split_once(';') {
            Some(("10", spec)) => self.report.fg = parse_color(spec),
            Some(("11", spec)) => self.report.bg = parse_color(spec),
            _ => {}
        }
        Step::CONSUMED
    }

    /// 一个 APC 回答的主体：kitty 图形成功时是 `G` + `i=31;OK`，出错时是 `i=31;E…`。
    fn apc(&mut self, body: &str) -> Step {
        if body.starts_with('G') && body.contains("i=31") {
            self.report.kitty_graphics = body.ends_with(";OK");
        }
        Step::CONSUMED
    }

    /// 收完一个 OSC / APC：取出主体，回到空闲。
    fn take_body(&mut self) -> String {
        let body = self.body();
        self.pending.clear();
        self.state = ReplyState::Idle;
        body
    }
}

impl Handshake for TerminalProbe {
    fn request(&self) -> String {
        let mut s = String::from(ansi::SYNC_BEGIN);
        s.push_str(&WidthProbe::queries());
        for slot in ["10", "11"] {
            s.push_str(ansi::OSC);
            s.push_str(slot);
            s.push_str(";?");
            s.push_str(ansi::BELL);
        }
        s.push_str(KITTY_QUERY);
        s.push_str(ansi::CSI);
        s.push_str("16t");
        s.push_str(ansi::CSI);
        s.push('c');
        s.push_str(ansi::CARRIAGE_RETURN);
        s.push_str(ansi::ERASE_TO_EOL);
        s.push_str(ansi::SYNC_END);
        s
    }

    fn feed(&mut self, b: u8) -> Step {
        match self.state {
            ReplyState::Idle => {
                if b == 0x1b {
                    self.pending.push(b);
                    self.state = ReplyState::AfterEsc;
                    Step::CONSUMED
                } else {
                    Step::leak(vec![b])
                }
            }
            ReplyState::AfterEsc => {
                let next = match b {
                    b'[' => ReplyState::Csi,
                    b']' => ReplyState::Osc,
                    b'_' => ReplyState::Apc,
                    _ => return self.release(b),
                };
                self.pending.push(b);
                self.state = next;
                Step::CONSUMED
            }
            ReplyState::Csi => {
                // CSI 序列里不会有控制字节：它不是回答。
                if b < 0x20 {
                    self.release(b)
                } else {
                    self.pending.push(b);
                    if (0x40..=0x7e).contains(&b) {
                        self.csi()
                    } else {
                        Step::CONSUMED
                    }
                }
            }
            ReplyState::Osc => match b {
                0x07 => {
                    let body = self.take_body();
                    self.osc(&body)
                }
                0x1b => {
                    self.state = ReplyState::OscAfterEsc;
                    Step::CONSUMED
                }
                _ => {
                    self.pending.push(b);
                    Step::CONSUMED
                }
            },
            ReplyState::OscAfterEsc => {
                if b == b'\\' {
                    let body = self.take_body();
                    self.osc(&body)
                } else {
                    self.pending.extend_from_slice(&[0x1b, b]);
                    self.state = ReplyState::Osc;
                    Step::CONSUMED
                }
            }
            ReplyState::Apc => {
                if b == 0x1b {
                    self.state = ReplyState::ApcAfterEsc;
                } else {
                    self.pending.push(b);
                }
                Step::CONSUMED
            }
            ReplyState::ApcAfterEsc => {
                if b == b'\\' {
                    let body = self.take_body();
                    self.apc(&body)
                } else {
                    self.pending.extend_from_slice(&[0x1b, b]);
                    self.state = ReplyState::Apc;
                    Step::CONSUMED
                }
            }
        }
    }

    fn settle(&mut self) -> Settled {
        let widths = self.widths.settle().widths;
        let replay = std::mem::take(&mut self.pending);
        self.state = ReplyState::Idle;
        if let Some(on_settled) = self.on_settled.take() {
            on_settled(self.report);
        }
        Settled { replay, widths }
    }
}

/// 按 `sep` 切开并去掉结尾的空段（Scala / Java 不带上限的 `split` 的行为）。
fn split_trimmed(s: &str, sep: char) -> Vec<&str> {
    let mut parts: Vec<&str> = s.split(sep).collect();
    while parts.last() == Some(&"") {
        parts.pop();
    }
    parts
}

/// `rgb:R/G/B`（或 `rgba:R/G/B/A`），每通道 1–4 位十六进制（XParseColor），缩放到 8 位。
pub fn parse_color(spec: &str) -> Option<Rgb> {
    let body = spec
        .strip_prefix("rgb:")
        .or_else(|| spec.strip_prefix("rgba:"))
        .unwrap_or("");
    let parts = split_trimmed(body, '/');
    let [r, g, b] = parts.get(..3)? else {
        return None;
    };
    Some(Rgb::new(channel(r)?, channel(g)?, channel(b)?))
}

/// 一个通道：1–4 位十六进制，按位数的满值缩放到 0..255，四舍五入。
fn channel(h: &str) -> Option<u8> {
    if h.is_empty() || h.len() > 4 || !h.bytes().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let v = u32::from_str_radix(h, 16).ok()?;
    let max = (1u32 << (4 * h.len())) - 1;
    Some((f64::from(v) * 255.0 / f64::from(max)).round() as u8)
}

/// 进入时请求的 kitty 键盘级别（docs/puffin-tui.md §4.3「按键解码」）：已知正确实现了协议的终端才请求，别处是
/// `None`——在那里打开会产生重复或认不出的键，所以按键保持传统的字节形式。`vars` 是入口注入的环境变量
/// （`Options.vars`）。
///
/// 3.7 之前的 iTerm2 只请求消歧义（级别 1）。原因：iTerm2 3.6 在 flag 8（所有键都用转义码）打开时，把输入法提交的
/// 文字交给 `ModernKeyMapper.transformedText`，那里读 `NSApp.currentEvent.keyCode`；拼音输入法组字时按 Caps Lock
/// 切换输入源，提交时当前事件不是按键事件，`keyCode` 抛异常，iTerm2 崩溃。不打开 flag 8 时那段代码直接返回原文。
/// 上游 a965d9537 修复，3.7.0 起包含。版本缺失或认不出时按旧版处理。
pub fn keyboard_enhancement(vars: &BTreeMap<String, String>) -> Option<&'static str> {
    let var = |k: &str| vars.get(k).map_or("", String::as_str);
    let term_program = var(env::TERM_PROGRAM);
    let term = var(env::TERM);
    if term_program == "iTerm.app" {
        if vars
            .get(env::TERM_PROGRAM_VERSION)
            .is_some_and(|v| at_least(v, 3, 7))
        {
            Some(ansi::PUSH_KEYBOARD_ENHANCEMENT)
        } else {
            Some(ansi::PUSH_KEYBOARD_DISAMBIGUATE)
        }
    } else if term_program == "WezTerm"
        || term_program == "ghostty"
        || term.contains("kitty")
        || term == "xterm-ghostty"
        || vars.contains_key(env::KITTY_WINDOW_ID)
    {
        Some(ansi::PUSH_KEYBOARD_ENHANCEMENT)
    } else {
        None
    }
}

/// `version` 是至少 `major.minor` 的正式版。正好这个版本的预发布（`3.7.0beta1`）不算。
fn at_least(version: &str, major: i32, minor: i32) -> bool {
    let parts = split_trimmed(version, '.');
    let [ma, mi, rest @ ..] = parts.as_slice() else {
        return false;
    };
    let (Ok(a), Ok(b)) = (ma.parse::<i32>(), mi.parse::<i32>()) else {
        return false;
    };
    a > major
        || (a == major && b > minor)
        || (a == major
            && b == minor
            && rest
                .first()
                .is_none_or(|r| r.bytes().all(|c| c.is_ascii_digit())))
}
