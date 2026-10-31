//! `/resume` 页的 footer（修复线 F29，docs/specs/rust-cli/tui-manage.md §10.2）：进度与两行按键提示，放不下时按优先级
//! 保留。F122 第二阶段从 `resume_draw.rs` 拆出（那个文件超过 800 行；规则的出处见 provenance/tui.md）。
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::i18n::Lang;
use super::i18n_projects::{self as words, CTRL};
use super::palette::Palette;
use super::resume::ResumePage;

/// footer 的提示用宽版说明的最小宽度。
const WIDE_FROM: usize = 120;
/// 提示之间的空格。
const GAP: usize = 3;

/// 一行放不下时按优先级保留提示：数字小的先留。
mod keep {
    pub const RESUME: u8 = 0;
    pub const ESC: u8 = 1;
    pub const RENAME: u8 = 2;
    pub const DELETE: u8 = 2;
    pub const QUIT: u8 = 2;
    pub const DENSITY: u8 = 3;
    pub const TRANSCRIPT: u8 = 4;
    pub const BROWSE: u8 = 5;
    pub const EXPAND: u8 = 6;
    pub const FOCUS: u8 = 7;
    pub const OPTION: u8 = 8;
}

/// footer 的一条提示。
pub(super) struct Hint {
    key: String,
    wide: &'static str,
    compact: &'static str,
    priority: u8,
}

#[derive(Clone, Copy)]
enum Mode {
    Wide,
    Compact,
    KeyOnly,
}

fn hint(lang: Lang, key: &str, which: &str, priority: u8) -> Hint {
    let (wide, compact) = words::footer_label(lang, which);
    Hint {
        key: key.to_owned(),
        wide,
        compact,
        priority,
    }
}

fn fit(hints: &[&Hint], mode: Mode, width: usize, palette: &Palette) -> Option<Line<'static>> {
    let label = |hint: &Hint| match mode {
        Mode::Wide => Some(hint.wide),
        Mode::Compact => Some(hint.compact),
        Mode::KeyOnly => None,
    };
    let total: usize = 1 + hints
        .iter()
        .enumerate()
        .map(|(index, hint)| {
            hint.key.width()
                + label(hint).map_or(0, |label| 1 + label.width())
                + if index > 0 { GAP } else { 0 }
        })
        .sum::<usize>();
    if total > width {
        return None;
    }
    let mut spans = vec![Span::raw(" ")];
    for (index, hint) in hints.iter().enumerate() {
        if index > 0 {
            spans.push(Span::raw(" ".repeat(GAP)));
        }
        spans.push(Span::styled(hint.key.clone(), palette.key()));
        if let Some(label) = label(hint) {
            spans.push(Span::styled(format!(" {label}"), palette.secondary()));
        }
    }
    Some(Line::from(spans))
}

/// 一行提示：宽版、窄版，放不下时按优先级保留能放下的最多几条。
pub(super) fn hint_row(hints: &[Hint], width: usize, palette: &Palette) -> Line<'static> {
    let all: Vec<&Hint> = hints.iter().collect();
    if width >= WIDE_FROM
        && let Some(line) = fit(&all, Mode::Wide, width, palette)
    {
        return line;
    }
    if let Some(line) = fit(&all, Mode::Compact, width, palette) {
        return line;
    }
    let mut kept: Vec<usize> = (0..hints.len()).collect();
    kept.sort_by_key(|&index| hints[index].priority);
    for mode in [Mode::Compact, Mode::KeyOnly] {
        for count in (1..=kept.len()).rev() {
            let mut chosen = kept[..count].to_vec();
            chosen.sort_unstable();
            let picked: Vec<&Hint> = chosen.iter().map(|&index| &hints[index]).collect();
            if let Some(line) = fit(&picked, mode, width, palette) {
                return line;
            }
        }
    }
    Line::default()
}

pub(super) fn footer_hints(lang: Lang, page: &ResumePage) -> [Vec<Hint>; 2] {
    let ctrl = |key: char| format!("{CTRL}{key}");
    let mut first = vec![hint(lang, "enter", "resume", keep::RESUME)];
    if !page.filtered().is_empty() {
        first.push(hint(lang, &ctrl('r'), "rename", keep::RENAME));
        first.push(hint(lang, &ctrl('d'), "delete", keep::DELETE));
    }
    let esc = match (page.query.is_empty(), page.startup) {
        (false, _) => "clear",
        (true, true) => "new",
        (true, false) => "exit",
    };
    first.push(hint(lang, "esc", esc, keep::ESC));
    first.push(hint(
        lang,
        &ctrl('c'),
        if page.startup { "quit" } else { "exit" },
        keep::QUIT,
    ));
    first.push(hint(lang, "tab", "focus", keep::FOCUS));
    first.push(hint(lang, "←/→", "option", keep::OPTION));
    let density = if page.comfortable {
        "dense"
    } else {
        "comfortable"
    };
    let second = vec![
        hint(lang, &ctrl('o'), density, keep::DENSITY),
        hint(lang, &ctrl('t'), "transcript", keep::TRANSCRIPT),
        hint(lang, &ctrl('e'), "expand", keep::EXPAND),
        hint(lang, "↑/↓", "browse", keep::BROWSE),
    ];
    [first, second]
}

/// 进度：` 位置 / 总数 · 百分比% `，放不下时依次变短。
pub(super) fn progress(page: &ResumePage, percent: u8, width: usize) -> String {
    let count = page.filtered().len();
    let position = if count == 0 { 0 } else { page.selected + 1 };
    let total = if page.rows.is_none() {
        format!("{count}…")
    } else {
        count.to_string()
    };
    [
        format!(" {position} / {total} · {percent}% "),
        format!(" {position}/{total} · {percent}% "),
        format!(" {percent}% "),
    ]
    .into_iter()
    .find(|label| label.width() < width)
    .unwrap_or_default()
}
