//! 设置页输入框上方的一行、状态栏右边的键与键入值时板上长出的那一行（docs/puffin-tui.md §4.2「设置」）。入参是中性的
//! [`Keys`]、[`Chosen`] 与说过的话；壳把自己的 `SettingsMode` 与带动作的行映射进来。
//! 移植自 Puffin 的 shell/settings/hints.rs（SettingsView.scala 的 `said`、`askingLine`、`keys`）与
//! settings/slab.rs 的 `settings_slab` 里键入值的那一行。
use crate::app::element::{Element, br, single_line};
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

/// 在等的请求是哪种。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Asking {
    Question,
    Plan,
    Permission,
}

/// 请求问的，输入框上方琥珀色的那一行。
pub fn asking_line(kind: Asking) -> &'static str {
    match kind {
        Asking::Question => "The agent has a question.",
        Asking::Plan => "The agent has a plan for you.",
        Asking::Permission => "The agent is asking for permission.",
    }
}

/// 输入框上方的一行：等着的请求（`asking`）、失败，或改动什么时候生效（`said`）。
pub fn said(theme: &Theme, said: Option<Result<&str, &str>>, asking: Option<&str>) -> Vec<Element> {
    let sp = |t: &str, s: Style| Span::new(t.to_owned(), s);
    if let Some(what) = asking {
        return vec![
            br(),
            single_line(
                vec![
                    sp(" ", Style::DEFAULT),
                    sp("●", Style::fg(theme.code)),
                    sp(" ", Style::DEFAULT),
                    sp(&format!("{what} "), theme.fg2),
                    sp("esc", theme.fg2),
                    sp(" goes back to answer it.", theme.fg3),
                ],
                false,
            ),
        ];
    }
    match said {
        Some(Err(err)) => vec![
            br(),
            single_line(
                vec![
                    sp(" ", Style::DEFAULT),
                    sp("✗", Style::fg(theme.err)),
                    sp(" ", Style::DEFAULT),
                    sp(err, theme.fg2),
                ],
                false,
            ),
        ],
        Some(Ok(text)) => vec![
            br(),
            single_line(vec![sp("   ", Style::DEFAULT), sp(text, theme.fg3)], false),
        ],
        None => vec![br()],
    }
}

/// 页在做的（状态栏的键按它）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keys {
    /// 输入框里的一个值；`stuck` 是草稿有错（规则少了结尾的 `)`），⏎ 不存。
    Editing {
        stuck: bool,
    },
    Choosing,
    Confirming,
    /// 输入框里的默认模型选择器。
    Picking,
    /// 在浏览器里等的登录。
    SigningIn,
    Searching,
    /// 焦点在段的目录上。
    Directory,
    /// 焦点在页上；`stacked` 是在段的子页上（esc 回上一级）。
    Page {
        stacked: bool,
    },
}

/// 空格在一项上做的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Space<'a> {
    None,
    /// 开关：现在开着没有。
    Toggle(bool),
    /// 随状态的说法（机器：连接、断开、停下，spec §11.1）。
    Word(&'a str),
}

/// 动作的行上 ⏎ 做的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Enter<'a> {
    /// 现在做不了：没有 ⏎。
    Off,
    /// 随状态的说法（机器）。
    Word(&'a str),
    /// 表单里的 `+` 行：用它。
    UseIt,
    /// 去添加规则的页。
    AddRule,
    Delete,
    Add,
    Run,
}

/// 选中的行，状态栏要的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Chosen<'a> {
    Switch {
        on: bool,
    },
    /// `open` 是 ⏎ 的说法（打开它的页时 `Some`，空串说成 `open`）。
    Item {
        space: Space<'a>,
        remove: bool,
        open: Option<&'a str>,
    },
    Text,
    Editor,
    Action(Enter<'a>),
    /// 其余能改的行：⏎ change。
    Other,
}

fn hint(k: &str, v: &str) -> (String, String) {
    (k.to_owned(), v.to_owned())
}

/// 页的状态下状态栏右边的键。`asking`：有请求在等。
pub fn keys(
    mode: Keys,
    chosen: Option<Chosen<'_>>,
    asking: bool,
    editor: &str,
    narrow: bool,
) -> Vec<(String, String)> {
    match mode {
        // 草稿有错（规则少了结尾的 `)`）时没有 ⏎（spec §11.9）。
        Keys::Editing { stuck: true } => vec![hint("esc", "cancel")],
        Keys::Editing { stuck: false } => vec![hint("⏎", "save"), hint("esc", "cancel")],
        Keys::Choosing => vec![hint("↑↓", "choose"), hint("⏎", "pick"), hint("esc", "back")],
        Keys::Confirming => vec![
            hint("↑↓", "choose"),
            hint("⏎", "answer"),
            hint("esc", "keep"),
        ],
        Keys::Picking => vec![
            hint("↑↓", "model"),
            hint("←→", "effort"),
            hint("⏎", "save"),
            hint("esc", "close"),
        ],
        Keys::SigningIn => vec![hint("esc", "cancel")],
        Keys::Searching => vec![
            hint("↑↓", "choose"),
            hint("⏎", "go there"),
            hint("esc", "clear"),
        ],
        Keys::Directory => vec![
            hint("↑↓", "section"),
            hint("→", "open"),
            hint("esc", if asking { "answer it" } else { "close" }),
        ],
        Keys::Page { stacked } => {
            let mut out = vec![hint("↑↓", "choose")];
            if !narrow {
                out.push(hint("←", "sections"));
            } else if !stacked {
                out.push(hint("←→", "section"));
            }
            match chosen {
                Some(Chosen::Switch { on }) => {
                    out.push(hint("⏎", if on { "turn off" } else { "turn on" }));
                }
                Some(Chosen::Item {
                    space,
                    remove,
                    open,
                }) => {
                    match space {
                        Space::Word(w) => out.push(hint("space", w)),
                        Space::Toggle(on) => {
                            out.push(hint("space", if on { "turn off" } else { "turn on" }));
                        }
                        Space::None => {}
                    }
                    if remove {
                        out.push(hint("⌫", "remove"));
                    }
                    if let Some(enter) = open {
                        out.push(hint("⏎", if enter.is_empty() { "open" } else { enter }));
                    }
                }
                Some(Chosen::Text) => out.push(hint("⏎", "edit")),
                Some(Chosen::Editor) => out.push(if editor.is_empty() {
                    hint("⏎", "edit")
                } else {
                    ("⏎".to_owned(), format!("edit in {editor}"))
                }),
                Some(Chosen::Action(e)) => match e {
                    Enter::Off => {}
                    Enter::Word(w) => out.push(hint("⏎", w)),
                    Enter::UseIt => out.push(hint("⏎", "use it")),
                    Enter::AddRule => out.push(hint("⏎", "add a rule")),
                    Enter::Delete => out.push(hint("⏎", "delete")),
                    Enter::Add => out.push(hint("⏎", "add")),
                    Enter::Run => out.push(hint("⏎", "run")),
                },
                Some(Chosen::Other) => out.push(hint("⏎", "change")),
                None => {}
            }
            let esc = if asking {
                "answer it"
            } else if stacked {
                "back"
            } else {
                "close"
            };
            out.push(hint("esc", esc));
            out
        }
    }
}

/// 设置页输入框的占位：搜索时。
pub const SEARCH_PLACEHOLDER: &str = "Search settings";

/// 键入一个值时输入框的占位：密钥、行自己的占位，或缺省的。`found` 是这一行是不是文字的值。
pub fn value_placeholder(found: bool, secret: bool, own: &str) -> &str {
    if !found {
        "Type the text"
    } else if secret {
        "Type or paste the key"
    } else if !own.is_empty() {
        own
    } else {
        "Type a value"
    }
}

/// 键入一个值时板上长出的那一行：名字（fg2），密钥时 `  hidden as you type`，有要填什么时 ` · ` 与它（fg3）。
/// 不带缩进：壳把它放进自己的行。
pub fn editing_row(theme: &Theme, label: &str, secret: bool, said: &str) -> Vec<Span> {
    let hint = if secret { "  hidden as you type" } else { "" };
    let mut grown = vec![
        Span::new(label.to_owned(), theme.fg2),
        Span::new(hint, theme.fg3),
    ];
    // 这一项要填什么（机器的 SSH 目标，spec §11.1）。
    if !said.is_empty() {
        grown.push(Span::new(" · ", theme.line));
        grown.push(Span::new(said.to_owned(), theme.fg3));
    }
    grown
}
