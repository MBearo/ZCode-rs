//! 设置页（docs/puffin-tui.md §4.2「设置」；docs/specs/rust-cli/tui-kit.md §4.4 K5）：页的墨、生效的时刻、段的目录，与
//! 页上的行作为中性的数据（[`Row`]）。两套界面各自把自己的页（带动作的行）映射成 [`Row`]，再交给 `view` 画。
//! 移植自 Puffin 的 shell/settings/data.rs（`Ink`、`Part`、`Applies`、`Sec`、`SECTIONS`、小工具）与
//! page.rs（`Opt`）；画法在 `view.rs`，状态栏的键与输入框上方的一行在 `hints.rs`，权限规则的措辞在 `rules.rs`。
use crate::render::width::string_width;
use crate::render::{Color, Span, Style, attr};
use crate::theme::theme::Theme;

use super::fit::clip;

pub mod hints;
pub mod rules;
pub mod view;

#[cfg(test)]
mod tests;

/// 段的目录从第 3 列起的宽度。
pub const INDEX_WIDTH: usize = 13;
/// 比这窄时页没有段的目录。
pub const NARROW: usize = 76;
/// 比这长的名字被截断。
pub const LABEL_MAX: usize = 30;
pub const NAME_MAX: usize = 22;

/// 页说出的墨；画法把它映到主题。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ink {
    Fg,
    Fg2,
    Fg3,
    Line,
    Brand,
    Ok,
    Err,
    Warn,
    Quiet,
}

/// 一段带墨的文字。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Part {
    pub text: String,
    pub ink: Ink,
    pub italic: bool,
}

impl Part {
    pub fn new(text: impl Into<String>, ink: Ink) -> Part {
        Part {
            text: text.into(),
            ink,
            italic: false,
        }
    }

    pub fn italic(text: impl Into<String>, ink: Ink) -> Part {
        Part {
            italic: true,
            ..Part::new(text, ink)
        }
    }
}

/// 改动什么时候生效：选中行下面说的，与保存之后输入框上方说的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Applies {
    Now,
    Next,
    /// 页上留着的值，等之后的动作添加那一项。
    Form,
    /// TUI 自己的样子（`theme.json`）：选的时候屏幕就变。
    Look,
}

impl Applies {
    pub fn short(self) -> &'static str {
        match self {
            Applies::Now => "Applies at once.",
            Applies::Next => "Applies to new sessions.",
            Applies::Form => "",
            Applies::Look => "Shows at once.",
        }
    }

    pub fn saved(self) -> &'static str {
        match self {
            Applies::Now => "Saved. The agent uses it from now on.",
            Applies::Next => "Saved. New sessions use it; this one keeps the old setting.",
            Applies::Form => "",
            Applies::Look => "Saved. It shows now.",
        }
    }
}

/// 选项：值、名字、说明、不能选的原因（"" 是能选）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Opt {
    pub value: String,
    pub label: String,
    pub desc: String,
    pub disabled: String,
}

impl Opt {
    pub fn new(value: impl Into<String>, label: impl Into<String>) -> Opt {
        Opt {
            value: value.into(),
            label: label.into(),
            desc: String::new(),
            disabled: String::new(),
        }
    }

    pub fn desc(mut self, desc: impl Into<String>) -> Opt {
        self.desc = desc.into();
        self
    }

    pub fn disabled(mut self, why: impl Into<String>) -> Opt {
        self.disabled = why.into();
        self
    }
}

/// 一段：id、名字、组（目录里两组之间空一行）、说明。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sec {
    pub id: &'static str,
    pub name: &'static str,
    pub group: u8,
    pub desc: &'static str,
}

/// 段的目录（spec §10.9：没有 Plugins；Permissions 在 General 之后，§11.9）。Memory 的说明换成规则文件（rust-cli 的 Memory 是它们）。
pub const SECTIONS: [Sec; 11] = [
    Sec {
        id: "general",
        name: "General",
        group: 0,
        desc: "How the agent works. These are the same settings as the desktop app's.",
    },
    Sec {
        id: "permissions",
        name: "Permissions",
        group: 0,
        desc: "What the agent may do without asking you. Your mode decides the rest; these rules make exceptions, and a Never runs rule wins over the others.",
    },
    Sec {
        id: "models",
        name: "Models",
        group: 0,
        desc: "Where the agent's models come from. Changes here apply to new sessions.",
    },
    Sec {
        id: "memory",
        name: "Memory",
        group: 0,
        desc: "Instruction files the agent reads at the start of a session.",
    },
    Sec {
        id: "appearance",
        name: "Appearance",
        group: 0,
        desc: "How ZCode looks in this terminal. Only the TUI has these; they are kept in theme.json.",
    },
    Sec {
        id: "subagents",
        name: "Subagents",
        group: 1,
        desc: "Agents the agent can hand a task to. Each is a Markdown file.",
    },
    Sec {
        id: "mcp",
        name: "MCP",
        group: 1,
        desc: "Tool servers the agent can call. A server that fails doesn't stop the agent; its tools are missing.",
    },
    Sec {
        id: "machines",
        name: "Machines",
        group: 1,
        desc: "Computers the agent can run its tools on, over SSH. A connected machine is offered to the agent's tools in every session; ■ means connected.",
    },
    Sec {
        id: "skills",
        name: "Skills",
        group: 1,
        desc: "Instructions the agent loads when a task needs them. Mention one in a message with $name.",
    },
    Sec {
        id: "commands",
        name: "Commands",
        group: 1,
        desc: "Your slash commands. Each is a Markdown file; its text is what the agent receives.",
    },
    Sec {
        id: "hooks",
        name: "Hooks",
        group: 1,
        desc: "Commands that run when something happens in a session. They run outside the sandbox.",
    },
];

/// id 为 `id` 的段；不认识的是第一段。
pub fn section(id: &str) -> &'static Sec {
    SECTIONS.iter().find(|s| s.id == id).unwrap_or(&SECTIONS[0])
}

/// 段在目录里的位置。
pub fn section_index(id: &str) -> Option<usize> {
    SECTIONS.iter().position(|s| s.id == id)
}

// ---- 页上的行（中性的数据：没有动作，只有画的时候要的） ----

/// 文字的值。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Text<'a> {
    pub label: &'a str,
    pub value: &'a str,
    pub desc: &'a str,
    pub applies: Applies,
    pub unit: &'a str,
    /// 空时的说法（fg3 斜体）。
    pub empty: &'a str,
    pub secret: bool,
    /// 值（或空时的说法）后面隔两格的说明（fg3）；没有时空。
    pub note: &'a str,
}

/// 清单里的一项：方块（`toggle`）、名字、来源列、状态。`opens`：⏎ 打开它的页（选中时行尾有 `›`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Item<'a> {
    pub name: &'a str,
    pub toggle: Option<bool>,
    pub scope: &'a str,
    pub status: &'a [Part],
    pub desc: &'a str,
    pub opens: bool,
    /// 选中时代替说明的一行（带墨的几段）；没有时空。
    pub detail: &'a [Part],
}

/// 动作的行：`plus` 是 `+ …` 的行，`danger` 是删除，`aside` 是名字后面的一列（fg3），`off` 是现在做不了（fg3）；
/// `signs_in` 是在浏览器里登录的那一行（登录时它下面说在等）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Action<'a> {
    pub label: &'a str,
    pub desc: &'a str,
    pub plus: bool,
    pub danger: bool,
    pub aside: &'a str,
    pub off: bool,
    pub signs_in: bool,
}

/// 页上的一行。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row<'a> {
    Heading(&'a str),
    Note(&'a str),
    Switch {
        label: &'a str,
        on: bool,
        desc: &'a str,
        applies: Applies,
    },
    Choice {
        label: &'a str,
        options: &'a [Opt],
        current: &'a str,
        default: &'a str,
        desc: &'a str,
        applies: Applies,
    },
    Text(Text<'a>),
    /// 模型的选择：（模型 id，档位）；没有设时 `None`。
    Picker {
        label: &'a str,
        value: Option<(&'a str, &'a str)>,
        desc: &'a str,
    },
    /// 一个文件：⏎ 在编辑器里打开它。
    Editor {
        label: &'a str,
        path: &'a str,
        desc: &'a str,
    },
    Info {
        label: &'a str,
        value: &'a [Part],
    },
    Item(Item<'a>),
    Action(Action<'a>),
}

impl Row<'_> {
    /// 能选的行：标题、说明与信息之外的。
    pub fn selectable(&self) -> bool {
        !matches!(self, Row::Heading(_) | Row::Note(_) | Row::Info { .. })
    }
}

// ---- 小工具（SettingsPage.scala 第 270–356 行） ----

pub fn plural(n: usize, word: &str) -> String {
    format!("{n} {word}{}", if n == 1 { "" } else { "s" })
}

/// `2.1 KB`、`640 B`。
pub fn size(bytes: u64) -> String {
    let b = bytes as f64;
    if bytes < 1024 {
        format!("{bytes} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.1} KB", b / 1024.0)
    } else {
        format!("{:.1} MB", b / 1024.0 / 1024.0)
    }
}

/// `128k`、`1M`：模型行说的上下文窗口。
pub fn tokens_short(n: u64) -> String {
    if n >= 1_000_000 && n.is_multiple_of(1_000_000) {
        format!("{}M", n / 1_000_000)
    } else if n >= 1000 {
        // 与 Scala 的 math.round 相同：.5 向上。
        format!("{}k", (n as f64 / 1000.0 + 0.5).floor() as u64)
    } else {
        n.to_string()
    }
}

/// 路径的最后一段。
pub fn file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// MCP 服务器的状态（投影的 `McpState` 与开关、登录的中性写法）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum McpStatus {
    Off,
    NeedsSignIn,
    Connected { tools: usize },
    Failed,
    Untrusted,
    Disconnected,
    Connecting,
}

/// 服务器的状态，行上的说法（Scala 的 `mcpStatusParts`）。
pub fn mcp_status_parts(s: McpStatus) -> Vec<Part> {
    match s {
        McpStatus::Off => vec![Part::new("off", Ink::Fg3)],
        McpStatus::NeedsSignIn => vec![Part::new("needs sign-in", Ink::Warn)],
        McpStatus::Connected { tools } => vec![
            Part::new("connected", Ink::Ok),
            Part::new(format!(" · {}", plural(tools, "tool")), Ink::Fg3),
        ],
        McpStatus::Failed => vec![Part::new("failed", Ink::Err)],
        McpStatus::Untrusted => vec![Part::new("needs review", Ink::Warn)],
        McpStatus::Disconnected => vec![Part::new("disconnected", Ink::Fg3)],
        McpStatus::Connecting => vec![Part::new("connecting", Ink::Fg3)],
    }
}

// ---- 墨与排版 ----

pub fn ink(theme: &Theme, i: Ink) -> Style {
    match i {
        Ink::Fg => Style::DEFAULT,
        Ink::Fg2 => theme.fg2,
        Ink::Fg3 => theme.fg3,
        Ink::Line => theme.line,
        Ink::Brand => Style::fg(theme.brand),
        Ink::Ok => Style::fg(theme.ok),
        Ink::Err => Style::fg(theme.err),
        Ink::Warn => Style::fg(theme.code),
        Ink::Quiet => theme.quiet,
    }
}

pub fn part_style(theme: &Theme, p: &Part) -> Style {
    let base = ink(theme, p.ink);
    if p.italic {
        base.with_attr(attr::ITALIC)
    } else {
        base
    }
}

pub fn spans(theme: &Theme, parts: &[Part]) -> Vec<Span> {
    parts
        .iter()
        .filter(|p| !p.text.is_empty())
        .map(|p| Span::new(p.text.clone(), part_style(theme, p)))
        .collect()
}

pub fn chosen_style(theme: &Theme) -> Style {
    if theme.rich {
        Style::fg(theme.brand)
    } else {
        Style::new(theme.brand, Color::Default, attr::BOLD)
    }
}

/// `text` 放得进 `cols` 格的末尾。
pub fn take_right_cells(text: &str, cols: usize) -> &str {
    let mut start = text.len();
    let mut w = 0;
    for (i, ch) in text.char_indices().rev() {
        let cw = string_width(&text[i..i + ch.len_utf8()]);
        if w + cw > cols {
            break;
        }
        w += cw;
        start = i;
    }
    &text[start..]
}

/// `text` 一个词一个词地装进宽 `width` 的行。
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut line = String::new();
    // 比一行还长的词（多是路径）从开头截去，留下结尾：结尾说得最多。
    let fit = |w: &str| {
        if string_width(w) <= width {
            w.to_owned()
        } else {
            format!("…{}", take_right_cells(w, width.saturating_sub(1).max(1)))
        }
    };
    for w0 in text.split(' ') {
        let w = fit(w0);
        if line.is_empty() {
            line = w;
        } else if string_width(&line) + 1 + string_width(&w) <= width {
            line.push(' ');
            line.push_str(&w);
        } else {
            out.push(std::mem::replace(&mut line, w));
        }
    }
    out.push(line);
    out
}

/// 截到 `n` 格的 span，最后一段以 `…` 结尾。
pub fn clip_spans(spans: Vec<Span>, n: usize) -> Vec<Span> {
    let mut out = Vec::new();
    let mut used = 0;
    for s in spans {
        let w = string_width(&s.text);
        if used + w <= n {
            used += w;
            out.push(s);
        } else {
            let room = n - used;
            if room > 0 {
                out.push(Span::new(clip(&s.text, room), s.style));
            }
            break;
        }
    }
    out
}
