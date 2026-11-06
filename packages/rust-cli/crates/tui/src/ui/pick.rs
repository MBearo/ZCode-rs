//! 选择列表的行（docs/specs/rust-cli/tui.md §7.4，修复线 F26；tui-kit.md §4.2「`/model`、`/mode`、`/effort` 等列表」，
//! 工作线 TK 的 S3）：选择列表照 Puffin 的层画——没有指针、没有底色条，选中的名字是 brand（plain 加粗），其余 fg2，说明
//! 选中 fg2、其余 fg3，`current` 是 fg3 的字；行从第 3 列起（docs/puffin-tui.md §4.2「网格」），量到 99 列；窗口与
//! `↑ n more` / `↓ n more` 同 Puffin `shell/consts.rs`；整层放在输入框的板上（rich：panel 底、半块上下边；plain：line 色
//! 的 `─` 上下线）。只产出 kit 的 [`Span`] / [`StyledLine`]，墨色经 [`Ink`]（tui-kit.md §3.1、§3.8）。
use rust_cli_tui_kit::app::element::{Element, Wrap, measure, single_line, stack, wrap_styled};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};

use super::ink::Ink;

/// Puffin 的网格：第 0 列边距、第 1 列标记、第 2 列空，正文从第 3 列起（kit `markdown::render::INDENT`）。
pub const INDENT: &str = "   ";
/// 一行量到的最大宽度与右边留的格数（Puffin `shell/consts.rs` 的 `MEASURE_MAX` / `MEASURE_MARGIN`）。
pub const MEASURE_MAX: usize = 99;
pub const MEASURE_MARGIN: usize = 2;
/// 选择器的窗口最多这么多行，`↑ n more` / `↓ n more` 算在内（Puffin `PICKER_ROWS`）。
pub const WINDOW: usize = 7;

/// 选中的名字：Puffin 的 `chosen_style`（rich 是 brand，plain 是 brand 加粗）。
pub fn chosen(ink: &Ink) -> Style {
    ink.selection()
}

/// 列表里的名字：选中 [`chosen`]，其余 fg2。
pub fn name_style(ink: &Ink, selected: bool) -> Style {
    if selected { chosen(ink) } else { ink.theme.fg2 }
}

/// 名字后面的说明：选中 fg2，其余 fg3。
pub fn desc_style(ink: &Ink, selected: bool) -> Style {
    if selected {
        ink.theme.fg2
    } else {
        ink.theme.fg3
    }
}

/// 缩进之后的一行（Puffin `asks/words.rs` 的 `row`）：量到 [`MEASURE_MAX`]，放不下时 `…` 截断。
pub fn row(spans: Vec<Span>) -> Element {
    let mut all = vec![Span::plain(INDENT)];
    all.extend(spans);
    measure(single_line(all, false), MEASURE_MAX, MEASURE_MARGIN)
}

/// fg3 的一行字（`↑ n more`、`No … matches`、空列表的说明）。
pub fn quiet_row(text: impl Into<String>, ink: &Ink) -> Element {
    row(vec![Span::new(text.into(), ink.theme.fg3)])
}

/// fg3 的一段说明，从第 3 列起按词折行（层里的副标题；Puffin 的说明行同样量到 [`MEASURE_MAX`]）。
pub fn quiet_wrap(text: impl Into<String>, ink: &Ink) -> Element {
    measure(
        wrap_styled(
            vec![Span::plain(INDENT)],
            vec![Span::plain(INDENT)],
            vec![Span::new(text.into(), ink.theme.fg3)],
            Wrap::Word,
        ),
        MEASURE_MAX,
        MEASURE_MARGIN,
    )
}

/// `text` 用空格补到 `cols` 格（Puffin `pad`）。
pub fn pad(text: &str, cols: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(cols.saturating_sub(string_width(text)))
    )
}

/// `text` 截到 `cols` 格，截断时最后一格是 `…`（Puffin `clip`）。
pub fn clip(text: &str, cols: usize) -> String {
    if string_width(text) <= cols {
        return text.to_owned();
    }
    let mut out = String::new();
    let mut used = 0;
    for ch in text.chars() {
        let w = string_width(ch.encode_utf8(&mut [0; 4]));
        if used + w < cols {
            out.push(ch);
            used += w;
        }
    }
    out.push('…');
    out
}

/// 没选中的名字：fg2，与 `filter` 相符的字母用正文的墨（Puffin `matched`）。小写改变长度的名字不标出。
pub fn matched(ink: &Ink, text: &str, filter: &str) -> Vec<Span> {
    let fg2 = ink.theme.fg2;
    let lower = text.to_lowercase();
    let at = (!filter.is_empty() && lower.len() == text.len())
        .then(|| lower.find(&filter.to_lowercase()))
        .flatten()
        .filter(|at| text.is_char_boundary(*at) && text.is_char_boundary(at + filter.len()));
    match at {
        None => vec![Span::new(text.to_owned(), fg2)],
        Some(at) => [
            Span::new(text[..at].to_owned(), fg2),
            Span::new(text[at..at + filter.len()].to_owned(), Style::DEFAULT),
            Span::new(text[at + filter.len()..].to_owned(), fg2),
        ]
        .into_iter()
        .filter(|span| !span.text.is_empty())
        .collect(),
    }
}

/// `count` 条选中 `sel` 时显示的第一条与条数（Puffin `picker_window`）：放得下就全部；否则 `sel` 周围的一个窗口，为它要的
/// 每个 `n more` 留一行。`rows` 是窗口的行数（Puffin 的选择器是 [`WINDOW`]）。
pub fn window(sel: usize, count: usize, rows: usize) -> (usize, usize) {
    let rows = rows.max(3);
    let place = |n: usize| -> usize { sel.saturating_sub(n / 2).min(count.saturating_sub(n)) };
    if count <= rows {
        (0, count)
    } else {
        let from = place(rows - 1);
        if from > 0 && from + rows - 1 < count {
            (place(rows - 2), rows - 2)
        } else {
            (from, rows - 1)
        }
    }
}

/// `↑ n more`（窗口上面藏着 `n` 条），前面空 `lead` 格（对齐到名字那一列）。
pub fn more_above(n: usize, lead: usize, ink: &Ink) -> Element {
    more(format!("↑ {n} more"), lead, ink)
}

/// `↓ n more`（窗口下面藏着 `n` 条）。
pub fn more_below(n: usize, lead: usize, ink: &Ink) -> Element {
    more(format!("↓ {n} more"), lead, ink)
}

fn more(text: String, lead: usize, ink: &Ink) -> Element {
    let mut spans = Vec::new();
    if lead > 0 {
        spans.push(Span::plain(" ".repeat(lead)));
    }
    spans.push(Span::new(text, ink.theme.fg3));
    row(spans)
}

/// 层的头一行（Puffin `/theme` 的头）：fg2 粗体的标题，三格后 fg3 的说明。
pub fn head(title: &str, note: Option<&str>, ink: &Ink) -> Element {
    let mut spans = vec![Span::new(
        title.to_owned(),
        ink.theme.fg2.with_attr(attr::BOLD),
    )];
    if let Some(note) = note.filter(|note| !note.is_empty()) {
        spans.push(Span::plain("   "));
        spans.push(Span::new(note.to_owned(), ink.theme.fg3));
    }
    row(spans)
}

/// 输入框的板上的一层（Puffin `shell/composer.rs`：输入框的板向上长出，没有边框和高亮条）：`rows` 按 `width` 排成的行，
/// 不带板的边。板只有一块，由 `pane.rs` 连同草稿行一起画（docs/specs/rust-cli/tui-kit.md §4.0「板」）；以前每个列表自己画
/// 一块板，替换整个底部区域。
pub fn layer(rows: Vec<Element>, _ink: &Ink, width: usize) -> Vec<StyledLine> {
    Layout::lay(&stack(rows), width.max(1))
}

/// 一组元素按 `width` 排成行（同画面测试与只要行的调用处用）。
pub fn lay(rows: &[Element], width: usize) -> Vec<StyledLine> {
    Layout::lay(&stack(rows.to_vec()), width.max(1))
}
