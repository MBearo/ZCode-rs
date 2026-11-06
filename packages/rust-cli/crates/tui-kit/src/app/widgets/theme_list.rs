//! `/theme` 的列表（docs/puffin-tui.md §4.2「主题」；docs/specs/rust-cli/tui-kit.md §4.4）：一行头（`Theme`，移动就显示、⏎
//! 留下、esc 放回打开时的那个），然后每个名字或 id 含过滤词的主题一行（名字、它的两边、打开时在用的说 `current`），至多
//! 9 行。纯的；入参是选中的与打开时的主题 id 与过滤词。
//! 移植自 Puffin 的 shell/theme_list.rs（`ROWS`、`matches`、`rows`）。
use crate::app::element::Element;
use crate::render::width::string_width;
use crate::render::{Span, attr};
use crate::theme::scheme::Scheme;
use crate::theme::schemes;
use crate::theme::theme::Theme;

use super::fit::pad;
use super::lists::{chosen_style, row, window};

/// 列表最多显示这么多行，`↑ n more` 与 `↓ n more` 算在内。
pub const ROWS: usize = 9;

/// 名字或 id 含 `filter` 的主题（不分大小写），按列表的次序。
pub fn matches(filter: &str) -> Vec<&'static str> {
    let f = filter.to_lowercase();
    schemes::ids()
        .iter()
        .copied()
        .filter(|id| {
            f.is_empty() || id.contains(&f) || schemes::name(id).to_lowercase().contains(&f)
        })
        .collect()
}

/// 列表的行：一行头，然后每个匹配 `filter` 的主题一行。`chosen` 是选中的主题，`was` 是列表打开时在用的。
pub fn rows(theme: &Theme, chosen: Option<&str>, was: &str, filter: &str) -> Vec<Element> {
    let head = row(vec![
        Span::new("Theme", theme.fg2.with_attr(attr::BOLD)),
        Span::plain("   "),
        Span::new(
            if theme.rich {
                format!(
                    "shows at once · ⏎ keeps it · esc puts back {}",
                    schemes::name(was)
                )
            } else {
                "this terminal shows no colours; it applies in a truecolour one".to_owned()
            },
            theme.fg3,
        ),
    ]);
    let ms = matches(filter);
    if ms.is_empty() {
        return vec![
            head,
            row(vec![Span::new(
                format!("No theme matches “{filter}”."),
                theme.fg3,
            )]),
        ];
    }
    let name_width = schemes::ids()
        .iter()
        .map(|id| string_width(schemes::name(id)))
        .max()
        .unwrap_or(0)
        + 3;
    let at = chosen.and_then(|c| ms.iter().position(|m| *m == c));
    let (from, until) = window(ms.len(), at.unwrap_or(0), ROWS);
    let more = |t: String| row(vec![Span::new(t, theme.fg3)]);
    let mut out = vec![head];
    if from > 0 {
        out.push(more(format!("↑ {from} more")));
    }
    for (i, id) in ms.iter().enumerate().take(until).skip(from) {
        let chosen = Some(i) == at;
        let sides =
            schemes::by_id(id).map_or_else(|| "follows your terminal".to_owned(), Scheme::sides);
        let mut spans = vec![
            Span::new(
                pad(schemes::name(id), name_width),
                if chosen {
                    chosen_style(theme)
                } else {
                    theme.fg2
                },
            ),
            Span::new(sides, if chosen { theme.fg2 } else { theme.fg3 }),
        ];
        if *id == was {
            spans.push(Span::new("   current", theme.fg3));
        }
        out.push(row(spans));
    }
    if until < ms.len() {
        out.push(more(format!("↓ {} more", ms.len() - until)));
    }
    out
}
