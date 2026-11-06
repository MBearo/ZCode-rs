//! 渲染器用的 ANSI / CSI / OSC 序列：常量与小的构造函数，都是纯的。
//! 移植自 packages/tui/tui/src/zcode/tui/render/Ansi.scala（feat/tui a2bf878b34）。
use rust_cli_base::hash::hex;

use super::color::Color;

pub const ESC: &str = "\x1b";
pub const CSI: &str = "\x1b[";
/// OSC 引导（`ESC ]`）。
pub const OSC: &str = "\x1b]";
/// BEL：兼容最广的 OSC 结尾（有的终端不认两字节的 ST）。
pub const BELL: &str = "\u{7}";
/// 字符串结尾，结束一个 OSC。
pub const ST: &str = "\x1b\\";

/// DEC 2026 同步输出：支持的终端把 Begin 与 End 之间的内容攒起来一次换上去，用户看不到画了一半的帧。不认识的终端
/// 忽略它，我们一帧一次写本身也够。
pub const SYNC_BEGIN: &str = "\x1b[?2026h";
pub const SYNC_END: &str = "\x1b[?2026l";

pub const HIDE_CURSOR: &str = "\x1b[?25l";
pub const SHOW_CURSOR: &str = "\x1b[?25h";

/// 进备用屏，存下光标（`CSI ?1049h`）：主屏与它的回滚不动。
pub const ALT_SCREEN_ENTER: &str = "\x1b[?1049h";
/// 回主屏，恢复光标（`CSI ?1049l`）。
pub const ALT_SCREEN_EXIT: &str = "\x1b[?1049l";

/// kitty 键盘协议：报告所有键、事件类型与关联文字；退出时恢复终端原来的键盘状态。不支持的终端忽略。
pub const PUSH_KEYBOARD_ENHANCEMENT: &str = "\x1b[>27u";
/// kitty 键盘协议只消歧义（标志 1）：带修饰的键仍是 `CSI u`，文字原样到。给 3.7 之前的 iTerm2：开着报告所有键（标志 8）
/// 时输入法上屏会崩（docs/puffin-tui.md §4.3「按键解码」）。
pub const PUSH_KEYBOARD_DISAMBIGUATE: &str = "\x1b[>1u";
pub const POP_KEYBOARD_ENHANCEMENT: &str = "\x1b[<u";

/// 鼠标报告：按键事件（`?1000`）、按住时的移动（`?1002`，拖动时持续报告位置）、SGR 1006 扩展坐标（`?1006`，列 / 行
/// 不限于 223，按下与松开分得开）。关的顺序与开的相反。没有 SGR 1006 的终端忽略。
pub const MOUSE_ENABLE: &str = "\x1b[?1000h\x1b[?1002h\x1b[?1006h";
pub const MOUSE_DISABLE: &str = "\x1b[?1006l\x1b[?1002l\x1b[?1000l";

/// 焦点报告：窗口得到 / 失去焦点时终端发 `CSI I` / `CSI O`（docs/puffin-tui.md §4.2「不在终端前时」）。
pub const FOCUS_ENABLE: &str = "\x1b[?1004h";
pub const FOCUS_DISABLE: &str = "\x1b[?1004l";
/// 窗口标题：存下现在的，设我们的，再弹回去。
pub const PUSH_TITLE: &str = "\x1b[22;0t";
pub const POP_TITLE: &str = "\x1b[23;0t";

/// 窗口标题（OSC 0）。标题是会话的标题（模型起的名字），只作数据写进去：见 [`osc_data`]。
pub fn set_title(title: &str) -> String {
    format!("\x1b]0;{}\u{7}", osc_data(title))
}

/// 放进 OSC 的外来文字：去掉全部控制字符（C0、DEL、C1）。
///
/// 缺陷修复（2026-10-30，MR 评审 CR-01）：标题与链接目标以前原样放进 OSC，里面的 BEL / ESC 会提前
/// 结束这个 OSC，后面的字节成为独立的终端命令（例如 `ESC ]52;c;…` 改剪贴板）。去掉控制字符之后它们只能是数据；只在这里
/// 清，不靠标题的生成与存储。
pub fn osc_data(text: &str) -> String {
    text.chars().filter(|c| !c.is_control()).collect()
}

/// 括号粘贴（`?2004`）：支持的终端把粘贴包在 `CSI 200~` … `CSI 201~` 里而不是当按键重放，解析器把一次粘贴作为一个事件，
/// 粘贴的换行是插入不是提交。
pub const BRACKETED_PASTE_ENABLE: &str = "\x1b[?2004h";
pub const BRACKETED_PASTE_DISABLE: &str = "\x1b[?2004l";

/// OSC 52 写剪贴板（`c` 选区），`base64` 是已编码的内容；BEL 结尾兼容最广。
pub fn osc52_copy(base64: &str) -> String {
    format!("{OSC}52;c;{base64}{BELL}")
}

/// 回到本行第 0 列：与待换行状态无关，所以水平定位先用它。
pub const CARRIAGE_RETURN: &str = "\r";
/// 从光标擦到行尾（`CSI 0K`）。
pub const ERASE_TO_EOL: &str = "\x1b[0K";
/// 擦整屏（`CSI 2J`）。
pub const CLEAR_SCREEN: &str = "\x1b[2J";
/// 光标到左上（`CSI H`）。
pub const CURSOR_HOME: &str = "\x1b[H";

pub fn cursor_up(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!("{CSI}{n}A")
    }
}

pub fn cursor_down(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!("{CSI}{n}B")
    }
}

pub fn cursor_right(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!("{CSI}{n}C")
    }
}

pub fn cursor_left(n: usize) -> String {
    if n == 0 {
        String::new()
    } else {
        format!("{CSI}{n}D")
    }
}

/// DECAWM 自动换行关 / 开（`CSI ?7l` / `?7h`）。备用屏里关着：每行都显式定位，换行只可能是终端对某个字形的宽度与
/// [`super::width`] 有分歧——在边缘裁掉让这一行的损坏留在行内，不把后面的行都推下去。每次离开备用屏时恢复：这个
/// 模式是全局的，不分屏。
pub const WRAP_OFF: &str = "\x1b[?7l";
pub const WRAP_ON: &str = "\x1b[?7h";

/// 重置全部 SGR（`CSI 0m`）。
pub const RESET: &str = "\x1b[0m";

/// 问终端的默认前景与背景（OSC 10/11）；回答是 `OSC 10;rgb:…` 与 `OSC 11;rgb:…`，BEL 或 ST 结尾。
pub const ASK_COLORS: &str = "\x1b]10;?\u{7}\x1b]11;?\u{7}";

/// 设终端的默认前景与背景（OSC 10/11），它自己的边缘与它擦出的东西都取主题的地（docs/puffin-tui.md §4.2「谁涂地」）。
/// 非真彩的颜色不写。
pub fn set_colors(fg: Color, bg: Color) -> String {
    let spec = |c: Color| match c {
        Color::TrueColor(r, g, b) => Some(format!("rgb:{}/{}/{}", hex(&[r]), hex(&[g]), hex(&[b]))),
        _ => None,
    };
    let mut out = String::new();
    if let Some(s) = spec(fg) {
        out.push_str(OSC);
        out.push_str("10;");
        out.push_str(&s);
        out.push_str(BELL);
    }
    if let Some(s) = spec(bg) {
        out.push_str(OSC);
        out.push_str("11;");
        out.push_str(&s);
        out.push_str(BELL);
    }
    out
}

/// 把终端的默认前景与背景还给它自己（OSC 110/111）。
pub const RESET_COLORS: &str = "\x1b]110\u{7}\x1b]111\u{7}";

/// DEC 2031：外观变化时终端报告 `CSI ? 997 ; 1 n`（深）或 `; 2 n`（浅）。不支持的忽略。
pub const APPEARANCE_REPORTS_ON: &str = "\x1b[?2031h";
pub const APPEARANCE_REPORTS_OFF: &str = "\x1b[?2031l";

/// 打开指向 `target` 的 OSC 8 链接：之后到 [`LINK_END`] 的文字在支持的终端里点击打开它；别的终端忽略。
pub fn link_start(target: &str) -> String {
    // 网址来自 Markdown（模型写的），只作数据：见 [`osc_data`]。
    format!("{OSC}8;;{}{ST}", osc_data(target))
}

/// 关上开着的 OSC 8 链接。SGR 重置不关它。
pub const LINK_END: &str = "\x1b]8;;\x1b\\";
