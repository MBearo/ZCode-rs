//! 模型选择器的行（docs/puffin-tui.md §4.2「模型（/model）」；docs/specs/rust-cli/tui-kit.md §4.4）：选中的周围一个窗口的
//! 模型（provider 一列只在一组的第一行，窄于 64 列时没有；选中的是 brand，过滤词的字母用 fg；在用的说 `current`），然后是
//! 选中的模型的档位（Steps 的条与档位的名字，名字换了时淡入）。纯的；入参是中性的 [`Model`] 与 [`Picker`]。
//! 移植自 Puffin 的 shell/picker.rs（`picker_rows`）与 shell/consts.rs（`PICKER_NARROW`、`PROVIDER_MAX`、
//! `is_off`、`bar_height`、`effort_bars`）。
use crate::app::element::Element;
use crate::render::width::string_width;
use crate::render::{Span, Style, attr};
use crate::theme::theme::Theme;

use super::fade;
use super::fit::{clip, pad};
use super::lists::{chosen_style, matched, picker_window, row};

/// 比这窄时选择器没有 provider 一列。
pub const PICKER_NARROW: usize = 64;
/// 更长的 provider 名用 `…` 截断。
pub const PROVIDER_MAX: usize = 20;

/// 关掉的档位没有条。
pub fn is_off(level: &str) -> bool {
    level == "disabled" || level == "none"
}

/// 自动档位（旧界面的选择器在最低一档之前加的一档，用户决定 2026-10-31；Puffin 没有）：选中时去掉档位，由模型自己定。
pub const AUTO: &str = "auto";

/// 有条的档位：关掉的与 `auto` 都没有条。
pub fn has_bar(level: &str) -> bool {
    !is_off(level) && level != AUTO
}

fn rank(level: &str) -> Option<i64> {
    Some(match level {
        "minimal" => 1,
        "low" => 2,
        "medium" => 3,
        "high" | "enabled" => 4,
        "xhigh" => 5,
        "max" => 6,
        _ => return None,
    })
}

const EIGHTHS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// 一个档位的条高，以格的八分之一计。按档位的意思固定，`high` 在每个模型里是同一根条；不认识的档位按它在列表里的位置。
pub fn bar_height(level: &str, i: usize, count: usize) -> i64 {
    match rank(level) {
        Some(r) => r + 2,
        None if count <= 1 => 8,
        // 与 Scala 的 math.round 相同：.5 向上。
        None => 3 + (5.0 * i as f64 / (count - 1) as f64 + 0.5).floor() as i64,
    }
}

/// 档位的控件（Steps，§4.2）：一个空格，每个有条的档位（[`has_bar`]）一根条、后面一个空格，再一个空格与档位的名字。`idx` 与它下面的
/// 亮着；`idx` 为 `None` 是没有档位。条不动；名字在变了 `named_age` tick 之后从板上淡入。
pub fn effort_bars(
    theme: &Theme,
    efforts: &[String],
    idx: Option<usize>,
    named_age: i64,
) -> Vec<Span> {
    effort_bars_said(theme, efforts, idx, named_age, NO_EFFORT)
}

/// 没有档位时的一句（Puffin 的英文）。
pub const NO_EFFORT: &str = "No effort setting for this model.";

/// [`effort_bars`]，没有档位时说 `none`（tui-kit.md §4.1 第 4 条：旧界面的中文传译文）。
pub fn effort_bars_said(
    theme: &Theme,
    efforts: &[String],
    idx: Option<usize>,
    named_age: i64,
    none: &str,
) -> Vec<Span> {
    let Some(idx) = idx.filter(|i| !efforts.is_empty() && *i < efforts.len()) else {
        return vec![Span::new(
            format!(" {none}"),
            theme.fg3.with_attr(attr::ITALIC),
        )];
    };
    let mut out = vec![Span::new(" ", Style::DEFAULT)];
    // 打头的 `auto` 不算进不认识的档位的位置（条高按有条的档位排）。
    let skip = usize::from(efforts.first().is_some_and(|l| l == AUTO));
    for (i, name) in efforts.iter().enumerate().filter(|(_, n)| has_bar(n)) {
        let on = i <= idx && has_bar(&efforts[idx]);
        let h = bar_height(name, i - skip, efforts.len() - skip).clamp(0, 8) as usize;
        let bar: String = std::iter::repeat_n(EIGHTHS[h], 2).collect();
        out.push(Span::new(
            bar,
            if on {
                Style::fg(theme.brand)
            } else {
                theme.line
            },
        ));
        out.push(Span::new(" ", Style::DEFAULT));
    }
    let name = &efforts[idx];
    let target = if is_off(name) { theme.fg3 } else { theme.fg2 };
    out.push(Span::new(" ", Style::DEFAULT));
    out.extend(fade::fade_in(
        theme,
        vec![Span::new(name.clone(), target)],
        named_age,
        fade::TICKS,
        theme.panel,
    ));
    out
}

/// 列出的一个模型。`current`：它是在用的模型。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Model<'a> {
    pub provider_id: &'a str,
    pub provider_name: &'a str,
    pub model_id: &'a str,
    pub current: bool,
}

/// 选择器此刻要画的。
///
/// - `any`：目录里有模型；`widest_provider`：目录里最宽的 provider 名（格），不只是匹配的。
/// - `matches`：id 或 provider 含过滤词的模型；`open`：选择器开着（关上时也说没有匹配）。
/// - `choice`：选中的行；`efforts` 与 `level`：选中的模型的档位与显示的那一个；`named_age`：档位的名字变了多少 tick。
#[derive(Clone, Copy, Debug)]
pub struct Picker<'a> {
    pub any: bool,
    pub widest_provider: usize,
    pub matches: &'a [Model<'a>],
    pub open: bool,
    pub choice: usize,
    pub filter: &'a str,
    pub efforts: &'a [String],
    pub level: Option<usize>,
    pub named_age: i64,
}

/// 选择器的行：选中的周围一个窗口的模型，然后是选中的模型的档位（§4.2）。
pub fn rows(theme: &Theme, p: &Picker<'_>, width: usize) -> Vec<Element> {
    let ms = p.matches;
    if !p.any {
        return vec![row(vec![Span::new(
            "No models are available yet.",
            theme.fg3,
        )])];
    }
    if !p.open || ms.is_empty() {
        return vec![row(vec![Span::new(
            format!("No model matches “{}”.", p.filter),
            theme.fg3,
        )])];
    }
    let pw = if width < PICKER_NARROW {
        0
    } else {
        p.widest_provider.min(PROVIDER_MAX) + 3
    };
    let nw = ms
        .iter()
        .map(|m| string_width(m.model_id))
        .max()
        .unwrap_or(0)
        + 3;
    let sel = p.choice.min(ms.len() - 1);
    let (from, n) = picker_window(sel, ms.len());
    let more = |text: String| {
        row(vec![
            Span::plain(" ".repeat(pw)),
            Span::new(text, theme.fg3),
        ])
    };
    let mut rows = Vec::new();
    if from > 0 {
        rows.push(more(format!("↑ {from} more")));
    }
    for i in from..from + n {
        let m = &ms[i];
        let group = i == from || ms[i - 1].provider_id != m.provider_id;
        let name0 = if group {
            clip(m.provider_name, PROVIDER_MAX)
        } else {
            String::new()
        };
        let mut spans = Vec::new();
        if pw > 0 {
            spans.push(Span::new(pad(&name0, pw), theme.fg3));
        }
        if i == sel {
            spans.push(Span::new(m.model_id.to_owned(), chosen_style(theme)));
        } else {
            spans.extend(matched(theme, m.model_id, p.filter));
        }
        if m.current {
            spans.push(Span::plain(
                " ".repeat(nw.saturating_sub(string_width(m.model_id))),
            ));
            spans.push(Span::new("current", theme.fg3));
        }
        rows.push(row(spans));
    }
    if from + n < ms.len() {
        rows.push(more(format!("↓ {} more", ms.len() - from - n)));
    }
    let label = Span::new(
        if pw == 0 {
            "Effort ".to_owned()
        } else {
            pad("Effort", pw - 1)
        },
        theme.fg3,
    );
    let mut effort = vec![label];
    effort.extend(effort_bars(theme, p.efforts, p.level, p.named_age));
    rows.push(row(effort));
    rows
}

#[cfg(test)]
#[path = "model_picker_tests.rs"]
mod tests;
