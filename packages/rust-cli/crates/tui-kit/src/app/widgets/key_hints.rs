//! 按键提示的排版（docs/puffin-tui.md §4.2「按键提示（槽）」「按键表（?）」）：槽的右沿、去掉明显的键、键的段、把槽放到
//! 一行的右沿、槽显示的（[`Slot`]）与它的淡入、按键表的列与键帽、Puffin 按键表的内容（[`columns`]）、换行与粘贴图片的键。
//! 槽说什么（规则的次序）在壳里（Puffin 壳的 shell/key_hints.rs），它对着壳的状态写；这里只有不带壳类型的部分，
//! 两套界面都调。
//! 移植自 packages/tui/tui/src/zcode/tui/shell/KeyHints.scala（feat/tui a2bf878b34）第 1–111 行。
//! 从 Puffin 的壳搬进 kit（docs/specs/rust-cli/tui-kit.md §4.4）；与壳里原来的不同只有一处：按键表的排版 [`sheet_of`] 收列，
//! 不自己造 Puffin 的列（壳的 `sheet` 给它 Puffin 的列）。行为不变。
use crate::app::element::{Element, br, by_width, single_line};
use crate::app::layout::Layout;
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::fade;

/// 槽的右沿（不含）：输入框的，比窗口少一格。输入框的板占满整个宽度；按正文栏（最多 99 列）对齐时，宽终端上槽停在框的
/// 中间偏右，所以跟框的右边走。
pub fn edge(width: usize) -> usize {
    width.saturating_sub(1)
}

/// 发送或选择的 ⏎：从不写出来。
const PLAIN_ENTER: [&str; 10] = [
    "send",
    "run",
    "answer",
    "pick",
    "choose",
    "switch",
    "open",
    "connect",
    "next",
    "change model",
];

/// 用终端的人都猜得到的键：⏎ 发送或选择，↑↓ 选择，esc 关上，^Q 退出。
pub fn obvious(key: &str, label: &str) -> bool {
    match key {
        "↑↓" | "^Q" => true,
        "esc" => label == "close",
        "⏎" => PLAIN_ENTER.contains(&label),
        _ => false,
    }
}

/// 槽里最多站这么多个键。
pub const MOST: usize = 2;

/// 一层的键按槽显示的样子：去掉明显的，最多两个。
pub fn pick(keys: &[(String, String)]) -> Vec<(String, String)> {
    keys.iter()
        .filter(|(k, l)| !obvious(k, l))
        .take(MOST)
        .cloned()
        .collect()
}

/// 键作为段：键 fg2，标签 fg3，两对之间两个空格。
pub fn spans(theme: &Theme, keys: &[(String, String)]) -> Vec<Span> {
    let mut out = Vec::new();
    for (i, (k, l)) in keys.iter().enumerate() {
        if i > 0 {
            out.push(Span::new("  ", Style::DEFAULT));
        }
        out.push(Span::new(k.clone(), theme.fg2));
        if !l.is_empty() {
            out.push(Span::new(format!(" {l}"), theme.fg3));
        }
    }
    out
}

fn cells(spans: &[Span]) -> usize {
    spans.iter().map(|s| string_width(&s.text)).sum()
}

/// `row` 在槽的右沿放上 `slot`：行是一行、在它之前留得出两格时；否则行照旧。
pub fn on_row(row: Element, slot: Vec<Span>) -> Element {
    if slot.is_empty() {
        return row;
    }
    by_width(move |w| {
        let lines = Layout::lay(&row, w);
        let slot_w = cells(&slot);
        match lines.as_slice() {
            [line] if line.width() + 2 + slot_w <= edge(w) => {
                let mut spans = line.spans.clone();
                spans.push(Span::new(
                    " ".repeat(edge(w) - line.width() - slot_w),
                    Style::DEFAULT,
                ));
                spans.extend(slot.iter().cloned());
                single_line(spans, false)
            }
            _ => row.clone(),
        }
    })
}

/// 按键表的一项：画成键帽的键，或写成词的鼠标手势。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub key: String,
    pub label: String,
    pub gesture: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub title: String,
    pub entries: Vec<Entry>,
}

/// 画成键帽的一项。
pub fn entry(key: &str, label: &str) -> Entry {
    Entry {
        key: key.to_owned(),
        label: label.to_owned(),
        gesture: false,
    }
}

/// 写成词的鼠标手势。
pub fn gesture(key: &str, label: &str) -> Entry {
    Entry {
        gesture: true,
        ..entry(key, label)
    }
}

/// 一组：标题与各项。
pub fn group(title: &str, entries: Vec<Entry>) -> Group {
    Group {
        title: title.to_owned(),
        entries,
    }
}

/// 一列这么多格宽。
pub const COLUMN_WIDTH: usize = 28;

/// 宽 `width` 时按键表 `columns`（每列一组或几组）的行：从第 3 列起放得下几列就并排几列，其余在空一行之后的下一带。
/// 列的内容是各界面自己的（Puffin 的在壳的 `key_hints::columns`）。
pub fn sheet_of(theme: &Theme, columns: &[Vec<Group>], width: usize) -> Vec<Element> {
    let cols: Vec<Vec<Vec<Span>>> = columns.iter().map(|c| column(theme, c)).collect();
    let per_band = (width.saturating_sub(3) / COLUMN_WIDTH).max(1);
    let mut out = Vec::new();
    for (b, band) in cols.chunks(per_band).enumerate() {
        if b > 0 {
            out.push(br());
        }
        let height = band.iter().map(Vec::len).max().unwrap_or(0);
        for r in 0..height {
            let mut row = vec![Span::new(INDENT, Style::DEFAULT)];
            for (i, col) in band.iter().enumerate() {
                let cell = col.get(r).cloned().unwrap_or_default();
                let w = cells(&cell);
                row.extend(cell);
                if i + 1 < band.len() {
                    row.push(Span::new(
                        " ".repeat(COLUMN_WIDTH.saturating_sub(w).max(1)),
                        Style::DEFAULT,
                    ));
                }
            }
            out.push(single_line(row, false));
        }
    }
    out
}

/// 一列的行：每组的标题与各项，两组之间空一行。
fn column(theme: &Theme, groups: &[Group]) -> Vec<Vec<Span>> {
    let mut out = Vec::new();
    for (i, g) in groups.iter().enumerate() {
        if i > 0 {
            out.push(Vec::new());
        }
        let cap_w = g
            .entries
            .iter()
            .map(|e| cells(&cap(theme, e)))
            .max()
            .unwrap_or(0);
        out.push(vec![Span::new(
            g.title.clone(),
            theme.fg2.with_attr(attr::BOLD),
        )]);
        for e in &g.entries {
            let mut row = cap(theme, e);
            let w = cells(&row);
            row.push(Span::new(" ".repeat(cap_w - w + 2), Style::DEFAULT));
            row.push(Span::new(e.label.clone(), theme.fg3));
            out.push(row);
        }
    }
    out
}

/// 键帽：line 的底、fg2、两边各一个空格；plain 模式没有底，键是终端墨色的粗体。手势是一个词，前面有键帽开头的那个空格，
/// 与键对齐。
fn cap(theme: &Theme, e: &Entry) -> Vec<Span> {
    if e.gesture {
        let lead = if theme.rich { " " } else { "" };
        vec![Span::new(format!("{lead}{}", e.key), theme.fg2)]
    } else if theme.rich {
        vec![Span::new(
            format!(" {} ", e.key),
            theme.fg2.with_bg(theme.line.fg_color()),
        )]
    } else {
        vec![Span::new(e.key.clone(), Style::attrs_only(attr::BOLD))]
    }
}

// ---- 槽显示的（K4 从壳搬来，docs/specs/rust-cli/tui-kit.md §4.4）----

/// 槽显示的：选好的键，或一句话（`says` 与它的墨色）。`at` 是它出现的 tick（按自己的时间淡入的）；`None` 时从界面记下的
/// 槽变化的时刻淡入。
#[derive(Clone, Debug, PartialEq, Default)]
pub struct Slot {
    pub keys: Vec<(String, String)>,
    pub says: Option<(String, Style)>,
    pub at: Option<i64>,
}

impl Slot {
    /// 原样的这几个键。
    pub fn keys(ks: &[(&str, &str)]) -> Slot {
        Slot {
            keys: pairs(ks),
            ..Slot::default()
        }
    }

    /// 一句话。
    pub fn says(text: String, ink: Style, at: Option<i64>) -> Slot {
        Slot {
            says: Some((text, ink)),
            at,
            ..Slot::default()
        }
    }
}

/// 键与说明的对，拥有的字。
pub fn pairs(ks: &[(&str, &str)]) -> Vec<(String, String)> {
    ks.iter()
        .map(|(k, l)| ((*k).to_owned(), (*l).to_owned()))
        .collect()
}

/// 槽的段（键或那句话），在变了 `age` tick 之后从板上淡入。
pub fn slot_spans(theme: &Theme, slot: &Slot, age: i64) -> Vec<Span> {
    let out = match &slot.says {
        Some((t, ink)) => vec![Span::new(t.clone(), *ink)],
        None => spans(theme, &slot.keys),
    };
    fade::fade_in(theme, out, age, fade::TICKS, theme.panel)
}

// ---- Puffin 按键表的内容（K4 从壳搬来）----

/// 换行的键：Terminal.app 分不出 ⇧⏎ 与 ⏎（§4.3）。
pub fn newline_key(apple_terminal: bool) -> &'static str {
    if apple_terminal { "⌥⏎" } else { "⇧⏎" }
}

/// 粘贴图片的键，状态栏这样说：`^V`，Windows 上 `Alt+V`。
pub fn paste_key() -> &'static str {
    if cfg!(windows) { "Alt+V" } else { "^V" }
}

/// 按键表的列，每列一组或几组（§4.2「按键表（?）」）：Writing、Typed first 与 Mouse、Anywhere。
pub fn columns(newline: &str, paste: &str) -> Vec<Vec<Group>> {
    vec![
        vec![group(
            "Writing",
            vec![
                entry(newline, "new line"),
                entry(paste, "paste an image"),
                entry("↑ ↓", "earlier messages"),
                entry("^W", "cut a word"),
                entry("^K", "cut to line end"),
                entry("^Y", "put back"),
            ],
        )],
        vec![
            group(
                "Typed first",
                vec![
                    entry("/", "a command"),
                    entry("@", "a file or session"),
                    entry("$", "a skill"),
                    entry("!", "a shell command"),
                ],
            ),
            group(
                "Mouse",
                vec![
                    gesture("drag", "select and copy"),
                    gesture("click", "open a link"),
                    gesture("wheel", "scroll"),
                ],
            ),
        ],
        vec![group(
            "Anywhere",
            vec![
                entry("⇧⇥", "permission mode"),
                entry("esc", "stop the turn"),
                entry("^O", "more details"),
                entry("^N", "back to the end"),
                entry("^Q", "quit"),
            ],
        )],
    ]
}

/// 宽 `width` 时 Puffin 的按键表的行（[`sheet_of`] 排 [`columns`]）。
pub fn sheet(theme: &Theme, newline: &str, paste: &str, width: usize) -> Vec<Element> {
    sheet_of(theme, &columns(newline, paste), width)
}
