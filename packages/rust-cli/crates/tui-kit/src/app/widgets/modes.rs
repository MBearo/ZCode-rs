//! 权限模式的名字、墨、说法与 `/mode` 的列表（docs/puffin-tui.md §4.2「权限模式」；docs/specs/rust-cli/tui-kit.md §4.4）。
//! 状态栏的模式、模式变了时输入框上方的一句、`/mode` 的四行（三个模式与计划模式，没有头，在用的说 `current`，计划模式说
//! `on` / `off`）。纯的；入参是中性的 [`Mode`]，两套界面各自把自己的模式映射过来。
//! 移植自 Puffin 的 shell/modes.rs（`mode_label`、`mode_style`、`mode_says`、`PLAN_*_SAYS`、`mode_parts`、
//! `MODE_ROWS`、`mode_list_rows`）。
use crate::app::element::Element;
use crate::render::{Span, Style};
use crate::theme::theme::Theme;

use super::fit::pad;
use super::lists::{chosen_style, row};

/// 四种权限模式：Ask before changes、Autonomous、Full access、Edit automatically。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    Build,
    Guarded,
    Yolo,
    Edit,
}

pub fn label(mode: Mode) -> &'static str {
    match mode {
        Mode::Build => "Ask before changes",
        Mode::Guarded => "Autonomous",
        Mode::Yolo => "Full access",
        Mode::Edit => "Edit automatically",
    }
}

/// Ask before changes 是 fg3，Autonomous 是 brand，Full access 是 amber。
pub fn style(theme: &Theme, mode: Mode) -> Style {
    match mode {
        Mode::Guarded => Style::fg(theme.brand),
        Mode::Yolo => Style::fg(theme.code),
        _ => theme.fg3,
    }
}

/// 模式变化时输入框上方的一行。
pub fn says(mode: Mode) -> &'static str {
    match mode {
        Mode::Build => "Ask before changes: it asks before each file change and each command.",
        Mode::Guarded => "Autonomous: it asks only before commands that can destroy work.",
        Mode::Yolo => "Full access: it runs commands and edits files without asking.",
        Mode::Edit => {
            "Edit automatically: it edits files without asking, and asks before commands."
        }
    }
}

pub const PLAN_ON_SAYS: &str =
    "Plan mode: it reads and plans, and changes nothing until you approve.";
pub const PLAN_OFF_SAYS: &str = "Plan mode is off.";

/// 状态栏的模式：计划模式开着时前面是 `Plan · `。
pub fn parts(theme: &Theme, mode: Mode, plan: bool) -> Vec<(String, Style)> {
    let mut out = Vec::new();
    if plan {
        out.push(("Plan".to_owned(), theme.fg2));
        out.push((" · ".to_owned(), theme.line));
    }
    out.push((label(mode).to_owned(), style(theme, mode)));
    out
}

/// `/mode` 列表的一行：三个模式，然后是计划模式（`mode` 为 `None`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ModeRow {
    pub mode: Option<Mode>,
    pub label: &'static str,
    pub desc: &'static str,
}

pub const MODE_ROWS: [ModeRow; 4] = [
    ModeRow {
        mode: Some(Mode::Build),
        label: "Ask before changes",
        desc: "Asks before each file change and command",
    },
    ModeRow {
        mode: Some(Mode::Guarded),
        label: "Autonomous",
        desc: "Asks only before commands that can destroy work",
    },
    ModeRow {
        mode: Some(Mode::Yolo),
        label: "Full access",
        desc: "Runs commands and edits files without asking",
    },
    ModeRow {
        mode: None,
        label: "Plan mode",
        desc: "Reads and plans; changes nothing until approved",
    },
];

/// `/mode` 列表的行；`current` 是在用的模式与计划模式开没开。
pub fn list_rows(theme: &Theme, choice: usize, current: Option<(Mode, bool)>) -> Vec<Element> {
    MODE_ROWS
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let chosen = i == choice;
            let name = Span::new(
                pad(r.label, 21),
                if chosen {
                    chosen_style(theme)
                } else {
                    theme.fg2
                },
            );
            let tag = match (r.mode, current) {
                (None, Some((_, true))) => "on",
                (None, _) => "off",
                (Some(m), Some((c, _))) if m == c => "current",
                _ => "",
            };
            let mut spans = vec![
                name,
                Span::new(r.desc, if chosen { theme.fg2 } else { theme.fg3 }),
            ];
            if !tag.is_empty() {
                spans.push(Span::new(format!("   {tag}"), theme.fg3));
            }
            row(spans)
        })
        .collect()
}
