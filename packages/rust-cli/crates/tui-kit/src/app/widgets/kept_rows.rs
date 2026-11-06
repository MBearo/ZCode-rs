//! 守门的几屏的行（docs/specs/rust-cli/workspace-trust.md、tui.md §7.8、update.md §6；docs/specs/rust-cli/tui-kit.md §4.4）：切到完全
//! 访问之前的确认框（标题、说明、三项、风险段、勾选框、取消与确认）、工作区信任的询问（标题与路径、安全检查、项目授权、
//! 两个回答、按键提示，连按退出时的提示）、新版本的选择屏。回答行与请求的回答同一画法：序号、名字，选中的是品牌色。
//! 纯的；入参是中性的焦点、[`Trust`] 与版本的字。
//! 移植自 Puffin 的 shell/kept/yolo.rs（常量、`YoloFocus`、`yolo_rows`、`yolo_slot`）、kept/trust.rs（常量、
//! `cancel`、`grants_title`、`list`、`sanitize`、`keys_row`、`trust_rows`）与 kept/update.rs（`TITLE`、`RELEASE_NOTES`、
//! `options`、`update_rows`）。
use crate::app::element::{Element, Wrap, br, single_line, wrap_styled};
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::fit::{measured, pad};
use super::lists::{chosen_style, row};

// ---- 完全访问的确认 ----

pub const YOLO_TITLE: &str = "Enable full access?";
pub const DESCRIPTION: &str =
    "Tasks can perform the following actions without asking for your approval each time.";
pub const ITEMS: [(&str, &str); 3] = [
    (
        "Files and folders",
        "Read, create, change, and delete accessible files.",
    ),
    (
        "Terminal commands",
        "Run commands, install software, or change system settings.",
    ),
    (
        "Network and apps",
        "Access the network, send data, and use connected apps.",
    ),
];
pub const RISK: &str = "These actions may cause data loss or disclosure. Enable only for tasks you trust. You can switch to another permission mode at any time.";
pub const ACKNOWLEDGEMENT: &str =
    "I understand the risks and take responsibility for my data security.";
pub const CANCEL: &str = "Cancel";
pub const YOLO_CONFIRM: &str = "Confirm";

/// 确认框的焦点所在的一处：勾选框、取消、确认（从上到下）；打开时在取消（App 同）。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum YoloFocus {
    Acknowledge,
    #[default]
    Cancel,
    Confirm,
}

impl YoloFocus {
    pub const ORDER: [YoloFocus; 3] = [
        YoloFocus::Acknowledge,
        YoloFocus::Cancel,
        YoloFocus::Confirm,
    ];

    /// ↑ / ↓ 移一处，到头回绕。
    pub fn step(self, down: bool) -> YoloFocus {
        let at = Self::ORDER.iter().position(|f| *f == self).unwrap_or(1);
        let next = if down {
            at + 1
        } else {
            at + Self::ORDER.len() - 1
        };
        Self::ORDER[next % Self::ORDER.len()]
    }
}

fn wrapped(spans: Vec<Span>, wrap: Wrap) -> Element {
    measured(wrap_styled(
        vec![Span::plain(INDENT)],
        vec![Span::plain(INDENT)],
        spans,
        wrap,
    ))
}

/// 确认框开着时槽说的键。
pub fn yolo_slot(focus: YoloFocus) -> Vec<(String, String)> {
    let k = |a: &str, b: &str| (a.to_owned(), b.to_owned());
    if focus == YoloFocus::Acknowledge {
        vec![k("space", "tick"), k("esc", "cancel")]
    } else {
        vec![k("1–2", "answer"), k("esc", "cancel")]
    }
}

/// 确认框的字（旧界面的中文界面换成自己的；英文是 [`YOLO_WORDS`]）。
#[derive(Clone, Copy, Debug)]
pub struct YoloWords<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub items: [(&'a str, &'a str); 3],
    pub risk: &'a str,
    pub acknowledgement: &'a str,
    pub cancel: &'a str,
    pub confirm: &'a str,
}

pub const YOLO_WORDS: YoloWords<'static> = YoloWords {
    title: YOLO_TITLE,
    description: DESCRIPTION,
    items: ITEMS,
    risk: RISK,
    acknowledgement: ACKNOWLEDGEMENT,
    cancel: CANCEL,
    confirm: YOLO_CONFIRM,
};

/// 确认框的行（请求的那一层）：标题、说明、三项、风险段、勾选框、取消与确认。`checked`：勾上了风险的确认。
pub fn yolo_rows(theme: &Theme, checked: bool, focus: YoloFocus) -> Vec<Element> {
    yolo_rows_said(theme, &YOLO_WORDS, checked, focus)
}

/// 同 [`yolo_rows`]，字由调用方给。
pub fn yolo_rows_said(
    theme: &Theme,
    words: &YoloWords<'_>,
    checked: bool,
    focus: YoloFocus,
) -> Vec<Element> {
    let chosen = chosen_style(theme);
    let mut rows = vec![
        row(vec![
            Span::new("⚠ ", Style::fg(theme.code)),
            Span::new(words.title.to_owned(), Style::DEFAULT),
        ]),
        wrapped(
            vec![Span::new(words.description.to_owned(), theme.fg2)],
            Wrap::Word,
        ),
    ];
    let column = words
        .items
        .iter()
        .map(|(name, _)| string_width(name))
        .max()
        .unwrap_or(0)
        + 3;
    rows.extend(words.items.iter().map(|(name, what)| {
        row(vec![
            Span::new(pad(name, column), theme.fg2),
            Span::new((*what).to_owned(), theme.fg3),
        ])
    }));
    rows.push(wrapped(
        vec![Span::new(words.risk.to_owned(), theme.fg3)],
        Wrap::Word,
    ));
    let on = |f: YoloFocus| focus == f;
    let tick = Span::new(
        if checked { "■ " } else { "□ " },
        if checked || on(YoloFocus::Acknowledge) {
            Style::fg(theme.brand)
        } else {
            theme.fg3
        },
    );
    rows.push(measured(wrap_styled(
        vec![Span::plain(INDENT), tick],
        vec![Span::plain(format!("{INDENT}  "))],
        vec![Span::new(
            words.acknowledgement.to_owned(),
            if on(YoloFocus::Acknowledge) {
                chosen
            } else {
                theme.fg2
            },
        )],
        Wrap::Word,
    )));
    let answer = |n: usize, label: &str, f: YoloFocus, ink: Style| {
        let num = Span::new(
            format!("{n}  "),
            if on(f) {
                Style::fg(theme.brand)
            } else {
                theme.fg3
            },
        );
        row(vec![
            num,
            Span::new(label.to_owned(), if on(f) { chosen } else { ink }),
        ])
    };
    rows.push(answer(1, words.cancel, YoloFocus::Cancel, theme.fg2));
    // 没勾选时确认不可用（line 色）；勾选后是 err 色（破坏性，旧 tui 的 destructive）。
    let confirm_ink = if checked {
        Style::fg(theme.err)
    } else {
        theme.line
    };
    rows.push(answer(2, words.confirm, YoloFocus::Confirm, confirm_ink));
    rows
}

// ---- 工作区信任 ----

pub const TRUST_TITLE: &str = "Accessing workspace:";
pub const SAFETY_CHECK: &str = "Quick safety check: Is this a project you created or one you trust? (Like your own code, a well-known open source project, or work from your team). If not, take a moment to review what's in this folder first.";
pub const ABILITIES: &str = "ZCode'll be able to read, edit, and execute files here.";
pub const GRANTS_NOTE: &str =
    "These will apply without asking. Only proceed if you trust this configuration.";
pub const TRUST_CONFIRM: &str = "Yes, I trust this folder";
/// 列出的规则最多几个。
const RULES_SHOWN: usize = 8;

/// 取消项：未信任时退出，后备询问时不带项目授权继续。
pub fn cancel(backstop: bool) -> &'static str {
    if backstop {
        "No, continue without these permissions"
    } else {
        "No, exit"
    }
}

/// 项目授权的标题（`This folder pre-approves <n> <tool permission(s)> in <来源>:`）。
pub fn grants_title(count: usize, sources: &str) -> String {
    let noun = if count == 1 {
        "tool permission"
    } else {
        "tool permissions"
    };
    format!("This folder pre-approves {count} {noun} in {sources}:")
}

/// 列表的写法：两项用 ` and `，三项以上用 `, ` 与 `, and `；超过 `limit` 时只列前 `limit` 项，其余写 `and <k> more`。
pub fn list(items: &[String], limit: Option<usize>) -> String {
    let shown = limit.filter(|limit| items.len() > *limit);
    let head = &items[..shown.unwrap_or(items.len())];
    match shown {
        None if items.len() == 1 => items[0].clone(),
        None if items.len() == 2 => format!("{} and {}", items[0], items[1]),
        None => match items.split_last() {
            Some((last, rest)) => format!("{}, and {last}", rest.join(", ")),
            None => String::new(),
        },
        Some(_) => {
            let more = items.len() - head.len();
            if head.len() == 1 {
                format!("{} and {more} more", head[0])
            } else {
                format!("{}, and {more} more", head.join(", "))
            }
        }
    }
}

/// 控制字符换成 U+FFFD。
fn sanitize(path: &str) -> String {
    path.chars()
        .map(|c| if c.is_control() { '\u{fffd}' } else { c })
        .collect()
}

/// 按键提示一行：键 fg2、说明 fg3，之间是 line 墨的 `  ·  `。
pub fn keys_row(theme: &Theme, keys: &[(&str, &str)]) -> Element {
    let mut spans = vec![Span::plain(INDENT)];
    for (i, (k, says)) in keys.iter().enumerate() {
        if i > 0 {
            spans.push(Span::new("  ·  ", theme.line));
        }
        spans.push(Span::new(k.to_string(), theme.fg2));
        spans.push(Span::new(format!(" {says}"), theme.fg3));
    }
    single_line(spans, false)
}

/// 项目级配置里会自动生效的授权：原始条数、排好去重的规则、写了它们的文件。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Grants<'a> {
    pub count: usize,
    pub rules: &'a [String],
    pub sources: &'a [String],
}

/// 信任的询问要显示的：工作区根的路径、是不是后备询问（取消项不带授权继续）、项目授权。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Trust<'a> {
    pub path: &'a str,
    pub backstop: bool,
    pub grants: Option<Grants<'a>>,
}

/// 询问的字（旧界面的中文界面换成自己的；英文是 [`trust_words`]）。`grants_title` 与 `grants_rules` 已经按项目授权写好，
/// 没有授权时不用。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TrustWords<'a> {
    pub title: &'a str,
    pub safety_check: &'a str,
    pub abilities: &'a str,
    pub grants_title: String,
    pub grants_rules: String,
    pub grants_note: &'a str,
    pub cancel: &'a str,
    pub confirm: &'a str,
    pub keys: [(&'a str, &'a str); 2],
    pub again: &'a str,
}

/// 英文的字（Puffin 的）。
pub fn trust_words(t: &Trust<'_>) -> TrustWords<'static> {
    let (grants_title, grants_rules) = t.grants.as_ref().map_or_else(Default::default, |g| {
        (
            grants_title(g.count, &list(g.sources, None)),
            list(g.rules, Some(RULES_SHOWN)),
        )
    });
    TrustWords {
        title: TRUST_TITLE,
        safety_check: SAFETY_CHECK,
        abilities: ABILITIES,
        grants_title,
        grants_rules,
        grants_note: GRANTS_NOTE,
        cancel: cancel(t.backstop),
        confirm: TRUST_CONFIRM,
        keys: [("⏎", "confirm"), ("esc", "cancel")],
        again: "again to exit",
    }
}

/// 询问的行（旧 tui 的 `Dialog::lines`）：标题与路径、安全检查、能做什么、项目授权（有时）、两个回答、按键提示。`focus`
/// 0 是取消项；`armed` 是连按退出的键（`C` 或 `D`），按了一次时最后一行说再按一次退出。
pub fn trust_rows(theme: &Theme, t: &Trust<'_>, focus: usize, armed: Option<char>) -> Vec<Element> {
    trust_rows_said(theme, t, &trust_words(t), focus, armed)
}

/// 同 [`trust_rows`]，字由调用方给。
pub fn trust_rows_said(
    theme: &Theme,
    t: &Trust<'_>,
    words: &TrustWords<'_>,
    focus: usize,
    armed: Option<char>,
) -> Vec<Element> {
    let para = |spans: Vec<Span>| wrapped(spans, Wrap::Word);
    let mut rows = vec![
        para(vec![Span::new(words.title.to_owned(), Style::DEFAULT)]),
        para(vec![Span::new(
            sanitize(t.path),
            Style::DEFAULT.with_attr(attr::BOLD),
        )]),
        br(),
        para(vec![Span::new(words.safety_check.to_owned(), theme.fg2)]),
        br(),
        para(vec![Span::new(words.abilities.to_owned(), theme.fg2)]),
    ];
    if t.grants.is_some() {
        rows.push(br());
        rows.push(para(vec![
            Span::new("⚠ ", Style::fg(theme.code)),
            Span::new(words.grants_title.clone(), Style::fg(theme.code)),
        ]));
        rows.push(para(vec![Span::new(
            format!("  {}", words.grants_rules),
            theme.fg2,
        )]));
        rows.push(para(vec![Span::new(
            words.grants_note.to_owned(),
            theme.fg3,
        )]));
    }
    rows.push(br());
    let chosen = chosen_style(theme);
    for (i, label) in [words.cancel, words.confirm].into_iter().enumerate() {
        let on = i == focus;
        rows.push(single_line(
            vec![
                Span::plain(INDENT),
                Span::new(
                    format!("{}  ", i + 1),
                    if on {
                        Style::fg(theme.brand)
                    } else {
                        theme.fg3
                    },
                ),
                Span::new(label.to_owned(), if on { chosen } else { theme.fg2 }),
            ],
            false,
        ));
    }
    rows.push(br());
    rows.push(match armed {
        Some(c) => keys_row(theme, &[(format!("^{c}").as_str(), words.again)]),
        None => keys_row(theme, &words.keys),
    });
    rows
}

// ---- 新版本 ----

pub const UPDATE_TITLE: &str = "Update available";
pub const RELEASE_NOTES: &str = "Release notes: ";

/// 三个选项：名字与说明。
pub fn options(command: &str) -> [(String, String); 3] {
    [
        ("Update now".to_owned(), format!("runs `{command}`")),
        ("Skip".to_owned(), String::new()),
        ("Skip until next version".to_owned(), String::new()),
    ]
}

/// 选择屏的行：标题与版本（`current → latest`）、更新说明的链接、三个选项（`command` 是更新跑的命令）、按键提示。
pub fn update_rows(
    theme: &Theme,
    current: &str,
    latest: &str,
    command: &str,
    notes_url: &str,
    focus: usize,
) -> Vec<Element> {
    let mut rows = vec![
        wrapped(
            vec![
                Span::new(UPDATE_TITLE, Style::fg(theme.brand).with_attr(attr::BOLD)),
                Span::new("   ", Style::DEFAULT),
                Span::new(format!("{current} → {latest}"), theme.fg3),
            ],
            Wrap::Char,
        ),
        wrapped(
            vec![
                Span::new(RELEASE_NOTES, theme.fg3),
                Span::new(notes_url.to_owned(), theme.fg2.with_attr(attr::UNDERLINE)),
            ],
            Wrap::Char,
        ),
        br(),
    ];
    let all = options(command);
    let lw = all.iter().map(|(l, _)| string_width(l)).max().unwrap_or(0) + 3;
    let chosen = chosen_style(theme);
    for (i, (label, desc)) in all.into_iter().enumerate() {
        let on = i == focus;
        let mut spans = vec![
            Span::plain(INDENT),
            Span::new(
                format!("{}  ", i + 1),
                if on {
                    Style::fg(theme.brand)
                } else {
                    theme.fg3
                },
            ),
            Span::new(pad(&label, lw), if on { chosen } else { theme.fg2 }),
        ];
        if !desc.is_empty() {
            spans.push(Span::new(desc, if on { theme.fg2 } else { theme.fg3 }));
        }
        rows.push(measured(single_line(spans, false)));
    }
    rows.push(br());
    rows.push(keys_row(theme, &[("⏎", "continue"), ("esc", "skip")]));
    rows
}
