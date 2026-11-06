//! 输入框上方的列表共用的写法（docs/puffin-tui.md §4.2「模型」「权限模式」「会话列表」；docs/specs/rust-cli/tui-kit.md §4.4）：
//! 缩进之后量过的一行、选中的墨、点亮的淡入、跟着选中的窗口、过滤词标出的字。两套界面的每个列表层都用它们。
//! 移植自 Puffin 的 shell/asks/words.rs（`chosen_style`、`row`、`fade_in`）、shell/consts.rs（`shelf_window`、
//! `picker_window`、`matched`、`take_right_cells`、`plain_text`）与 shell/rewind/words.rs（`found`）。
use crate::app::element::{Element, single_line};
use crate::markdown::render::INDENT;
use crate::render::width::string_width;
use crate::render::{Color, Span, Style, attr};
use crate::theme::rgb::Rgb;
use crate::theme::theme::Theme;

use super::fit::measured;

/// 模型与会话的列表最多这么多行，含 more 行。
pub const PICKER_ROWS: usize = 7;

/// 选中的行的墨：rich 是 brand，plain 是 brand 加粗。
pub fn chosen_style(theme: &Theme) -> Style {
    if theme.rich {
        Style::fg(theme.brand)
    } else {
        Style::new(theme.brand, Color::Default, attr::BOLD)
    }
}

/// 缩进之后的一行，量到 [`super::fit::MEASURE_MAX`]。
pub fn row(spans: Vec<Span>) -> Element {
    let mut all = vec![Span::plain(INDENT)];
    all.extend(spans);
    measured(single_line(all, false))
}

fn ease(p: f64) -> f64 {
    1.0 - (1.0 - p.clamp(0.0, 1.0)).powi(2)
}

/// `st` 从 line 色走到自己颜色的 `p`（回答的点亮、记号的淡入）。plain 模式完成之前是 dim。
pub fn fade_in(theme: &Theme, st: Style, p: f64) -> Style {
    if p >= 1.0 {
        return st;
    }
    if !theme.rich {
        return st.with_attr(attr::DIM);
    }
    let to = match st.fg_color() {
        Color::TrueColor(r, g, b) => Some(Rgb::new(r, g, b)),
        _ => theme.fg,
    };
    match (theme.line.fg_color(), to) {
        (Color::TrueColor(r, g, b), Some(to)) => {
            st.with_fg(Rgb::new(r, g, b).mix(to, ease(p)).color())
        }
        _ => st,
    }
}

/// 在 `rows` 行里、选中 `choice` 时显示 `n` 行中的 `[from, until)`：两端各有藏起的行时各占一行 more（命令架、主题、`@`）。
pub fn window(n: usize, choice: usize, rows: usize) -> (usize, usize) {
    if n <= rows {
        (0, n)
    } else if choice + 1 < rows {
        (0, rows - 1)
    } else if choice + rows > n {
        (n + 1 - rows, n)
    } else {
        // 两头都有「more」行：中间 rows - 2 行，选中的行在其中靠下。
        let from = choice + 3 - rows;
        (from, from + rows - 2)
    }
}

/// `count` 项选中 `sel` 时显示的第一行与行数：放得下就全部；否则 `sel` 周围的一个窗口，为它要的每个 `n more` 留一行
/// （模型、会话、回退的点）。
pub fn picker_window(sel: usize, count: usize) -> (usize, usize) {
    let place = |n: usize| -> usize {
        let centred = sel.saturating_sub(n / 2);
        centred.min(count.saturating_sub(n))
    };
    if count <= PICKER_ROWS {
        (0, count)
    } else {
        let from = place(PICKER_ROWS - 1);
        if from > 0 && from + PICKER_ROWS - 1 < count {
            (place(PICKER_ROWS - 2), PICKER_ROWS - 2)
        } else {
            (from, PICKER_ROWS - 1)
        }
    }
}

/// 没选中的 id：fg2，与 `filter` 相符的字母用 fg。小写改变长度的 id 不标出。
pub fn matched(theme: &Theme, id: &str, filter: &str) -> Vec<Span> {
    found(id, filter, theme.fg2, Style::DEFAULT)
}

/// `text` 用 `base`，其中的搜索词（不分大小写，第一处）用 `hit`；小写改变长度的字不标出。
pub fn found(text: &str, query: &str, base: Style, hit: Style) -> Vec<Span> {
    let lower = text.to_lowercase();
    let at = (!query.is_empty() && lower.len() == text.len())
        .then(|| lower.find(&query.to_lowercase()))
        .flatten()
        .filter(|at| text.is_char_boundary(*at) && text.is_char_boundary(at + query.len()));
    match at {
        None => vec![Span::new(text.to_owned(), base)],
        Some(at) => [
            Span::new(text[..at].to_owned(), base),
            Span::new(text[at..at + query.len()].to_owned(), hit),
            Span::new(text[at + query.len()..].to_owned(), base),
        ]
        .into_iter()
        .filter(|s| !s.text.is_empty())
        .collect(),
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

/// 部分的字连起来。
pub fn plain_text(parts: &[(String, Style)]) -> String {
    parts.iter().map(|(t, _)| t.as_str()).collect()
}
