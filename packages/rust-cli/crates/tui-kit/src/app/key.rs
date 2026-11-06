//! 解码后的按键（docs/puffin-tui.md §4.3「按键解码」）：应用绑定的键，以及终端在启动之后送来的几种报告（焦点、外观、
//! 颜色、鼠标）。别的都解成 [`Key::Unknown`]，应用忽略它。
//! 移植自 packages/tui/tui/src/zcode/tui/app/Key.scala（feat/tui a2bf878b34）。
//!
//! 与 Scala 的不同：Scala 的 `Char` 只能放一个 UTF-16 单元，高于 U+FFFF
//! 的码点（emoji）只好放进 `Text`。Rust 的 `char` 就是码点，所以打出的任何一个码点都是 [`Key::Char`]；
//! [`Key::Text`] 只留给一次插入多个码点的场合（壳与基准剧本用 `Key::Text("hi")`），解析器自己不产出它。
use crate::theme::Rgb;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Key {
    /// 打出的一个码点（含高于 U+FFFF 的 emoji）。
    Char(char),
    /// 一次插入的多个码点。解析器不产出，壳与基准剧本用。
    Text(String),
    Enter,
    Newline,
    /// 括号粘贴（`CSI 200~` … `CSI 201~`）整段送到：换行统一成 `\n`，其他控制字符丢掉。作为一个键到达（从不重放成
    /// 一串按键），粘贴里的换行才不会触发 Enter 绑定的动作。
    Paste(String),
    Backspace,
    Tab,
    /// Shift+Tab：传统的 `CSI Z`，或 kitty 协议的 `CSI 9;2u`。
    BackTab,
    Delete,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// emacs 式的控制键；字符是大写字母（如 `Ctrl('Q')`）。
    Ctrl(char),
    /// 按着 Alt（⌥）的键：`Alt(Char('b'))`、`Alt(Left)`、`Alt(Backspace)`、`Alt(Enter)`、`Alt(Delete)`。
    Alt(Box<Key>),
    /// 按着 Ctrl 的非字母键（`WithCtrl(Left)`）；Ctrl+字母仍是 [`Key::Ctrl`]。
    WithCtrl(Box<Key>),
    /// 按着 super（macOS 的 ⌘）的键；只有 kitty 协议的终端报告它。
    Super(Box<Key>),
    Esc,
    Unknown,
    /// 终端得到（`CSI I`）或失去（`CSI O`）焦点；`CSI ?1004h` 之后才报告。
    FocusIn,
    FocusOut,
    /// 终端的外观变了（DEC 2031 `CSI ? 997 ; 1|2 n`，`CSI ?2031h` 之后报告）：1 是 `dark`。不是按键
    /// （docs/puffin-tui.md §4.2「主题」）。
    Appearance {
        dark: bool,
    },
    /// 启动探测之后问的 OSC 10（`slot` 10，缺省前景）或 OSC 11（背景）的回答。不是按键。
    ColorReport {
        slot: u8,
        rgb: Rgb,
    },
    /// 鼠标滚轮的一格。`col` / `row` 是从 1 数起的终端格子（SGR 1006）。
    WheelUp {
        col: usize,
        row: usize,
    },
    WheelDown {
        col: usize,
        row: usize,
    },
    /// 鼠标按下。`button` 是 SGR 按键码的低两位（0 左、1 中、2 右）；`col` / `row` 从 1 数起；`mods` 是按着的 SGR
    /// 修饰位（4 shift、8 meta / Alt、16 ctrl），没有时为 0。
    MousePress {
        button: u8,
        col: usize,
        row: usize,
        mods: u8,
    },
    /// 鼠标松开，字段同 [`Key::MousePress`]。
    MouseRelease {
        button: u8,
        col: usize,
        row: usize,
        mods: u8,
    },
    /// 按着 `button` 移动（拖动）。不按键的悬停移动（`?1003`）不报告。
    MouseDrag {
        button: u8,
        col: usize,
        row: usize,
    },
}

impl Key {
    /// `Alt(key)`，省去 `Box::new`。
    pub fn alt(key: Key) -> Key {
        Key::Alt(Box::new(key))
    }

    /// `WithCtrl(key)`，省去 `Box::new`。
    pub fn with_ctrl(key: Key) -> Key {
        Key::WithCtrl(Box::new(key))
    }

    /// `Super(key)`，省去 `Box::new`（`super` 是关键字，所以叫这个名字）。
    pub fn with_super(key: Key) -> Key {
        Key::Super(Box::new(key))
    }
}
