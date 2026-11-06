//! 门道占着板的两步（docs/puffin-tui.md §4.2「密钥」）：粘贴密钥（名字一行，输入行一字一个 `•`）与别的 provider 的列表
//! （提示、至多 7 个模板、`+ Custom provider`、筛选的输入行），与门道各步的键（§4.2「按键」）。
//! 移植自 Puffin 的 shell/doorway/view.rs 的 `door_slab` 与 `door_keys`。
use crate::app::element::{Element, single_line};
use crate::app::frame::PROMPT_ARROW;
use crate::render::{Color, Span, Style, attr};
use crate::theme::theme::Theme;

use super::{PROVIDER_ROWS, chosen_style, span};
use crate::app::widgets::fit::pad;

fn chevron(theme: &Theme) -> Vec<Span> {
    vec![
        span(" ", Style::DEFAULT),
        span(
            PROMPT_ARROW,
            Style::new(theme.brand, Color::Default, attr::BOLD),
        ),
        span(" ", Style::DEFAULT),
    ]
}

/// 修复原因：门道写于光标格带 Attr.Caret 之前，这一格只是反显，门道占着板时没有带标记的格，隐藏的终端光标停在别处，
/// 输入法的候选窗不在密钥或筛选的输入行（§4.4「终端光标」）。
fn cursor() -> Span {
    span(" ", Style::CARET)
}

/// 粘贴 `name` 的密钥：名字一行，然后是输入行（打了 `typed` 个字）。
pub fn paste_rows(theme: &Theme, name: &str, typed: usize, width: usize) -> Vec<Element> {
    let indent = span("   ", Style::DEFAULT);
    // 一字一个 `•`：只说出长度，不说出内容。
    let dots = "•".repeat(typed.min(width.saturating_sub(30)));
    let after = if typed == 0 {
        span("Paste the key", theme.fg3)
    } else {
        span("  hidden as you type", theme.fg3)
    };
    let mut input = chevron(theme);
    input.extend([span(dots, Style::DEFAULT), cursor(), after]);
    vec![
        single_line(vec![indent, span(format!("{name} key"), theme.fg2)], false),
        single_line(input, false),
    ]
}

/// 别的 provider 的列表：`list` 是相符的（名字，主机），`name_w` 是所有别的 provider 的名字中最宽的；`custom` 时
/// 末尾有 `+ Custom provider`（选中它时 `sel == list.len()`）；最后一行是筛选的输入行。
pub fn providers_rows(
    theme: &Theme,
    list: &[(&str, &str)],
    name_w: usize,
    sel: usize,
    custom: bool,
    filter: &str,
) -> Vec<Element> {
    let indent = span("   ", Style::DEFAULT);
    let top = (sel as isize - 3)
        .min(list.len() as isize - PROVIDER_ROWS as isize)
        .max(0) as usize;
    let chosen = chosen_style(theme);
    let mut rows = vec![single_line(
        vec![
            indent.clone(),
            span("Pick the provider your key is for", theme.fg3),
        ],
        false,
    )];
    rows.extend(
        list.iter()
            .enumerate()
            .skip(top)
            .take(PROVIDER_ROWS)
            .map(|(i, (name, host))| {
                let on = i == sel;
                single_line(
                    vec![
                        indent.clone(),
                        span(pad(name, name_w), if on { chosen } else { theme.fg2 }),
                        span("   ", Style::DEFAULT),
                        span(*host, if on { theme.fg2 } else { theme.fg3 }),
                    ],
                    false,
                )
            }),
    );
    if custom {
        rows.push(single_line(
            vec![
                indent,
                span(
                    "+ Custom provider",
                    if sel == list.len() { chosen } else { theme.fg3 },
                ),
                span("   ", Style::DEFAULT),
                span("opens /settings", theme.line),
            ],
            false,
        ));
    }
    let mut input = chevron(theme);
    if filter.is_empty() {
        input.extend([cursor(), span("Search providers", theme.fg3)]);
    } else {
        input.extend([span(filter, Style::DEFAULT), cursor()]);
    }
    rows.push(single_line(input, false));
    rows
}

/// 门道在哪一步，键要的。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyStep {
    Choose,
    NoPlan {
        waiting: bool,
    },
    /// `switching`：在写连接、等模型。
    Teams {
        switching: bool,
    },
    KeyFor,
    SignIn {
        failed: bool,
    },
    Providers,
    Paste {
        checking: bool,
    },
}

/// 门道的键，槽从中挑（§4.2「按键」）。`empty`：输入框是空的；`ready`：agent 已就绪（门道是 `/login` 打开的）。
pub fn keys(step: KeyStep, empty: bool, ready: bool) -> Vec<(String, String)> {
    let ks: &[(&str, &str)] = match step {
        KeyStep::Choose if ready => &[
            ("↑↓", "choose"),
            ("⏎", if empty { "connect" } else { "send" }),
            ("esc", "close"),
            ("^Q", "quit"),
        ],
        KeyStep::Choose
        | KeyStep::NoPlan { waiting: false }
        | KeyStep::Teams { switching: false } => &[
            ("↑↓", "choose"),
            ("⏎", if empty { "connect" } else { "queue message" }),
            ("^Q", "quit"),
        ],
        KeyStep::NoPlan { waiting: true } | KeyStep::Teams { .. } => {
            &[("esc", "back"), ("^Q", "quit")]
        }
        KeyStep::KeyFor => &[
            ("↑↓", "choose"),
            ("⏎", "pick"),
            ("esc", "back"),
            ("^Q", "quit"),
        ],
        KeyStep::SignIn { failed: true } => &[("⏎", "try again"), ("esc", "back"), ("^Q", "quit")],
        KeyStep::SignIn { failed: false } => &[("c", "copy link"), ("esc", "back"), ("^Q", "quit")],
        KeyStep::Providers => &[("↑↓", "choose"), ("⏎", "pick"), ("esc", "back")],
        KeyStep::Paste { checking: true } => &[("esc", "cancel")],
        KeyStep::Paste { checking: false } => &[("⏎", "check key"), ("esc", "back")],
    };
    ks.iter()
        .map(|(k, l)| ((*k).to_owned(), (*l).to_owned()))
        .collect()
}
