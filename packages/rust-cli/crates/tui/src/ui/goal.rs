//! 对话里的 `goal_status` 行（docs/specs/rust-cli/goal.md §6）。文字有中英文。`/goal` 的卡在 `cards.rs`（kit 的
//! `goal::card`，tui-kit.md §4.2「`/goal`」）。
//!
//! 画法（tui-kit.md §3.8）：产出 kit 的 [`StyledLine`]，墨色只经 `Ink` 的 token。检查行照 Puffin 的检查行
//! （docs/puffin-tui.md §4.2「目标（/goal）」，`shell/goals.rs`）：第 1 列是记号（达成 `✓` ok、无法达成 `✗` err、继续 `◎`
//! brand），第 3 列起是句子（达成与无法达成 fg2 粗体，继续 fg3）与 fg3 的统计；无法达成的理由在第 3 列 line 色的 `│`
//! 后面，第 5 列起 fg2 折行。
use rust_cli_kernel_api::goal::GoalStatus;
use rust_cli_tui_kit::app::element::{Element, Wrap, measure, single_line, stack, wrap_styled};
use rust_cli_tui_kit::app::layout::Layout;
use rust_cli_tui_kit::render::{Span, Style, StyledLine, attr};

use super::cells::Ctx;
use super::i18n::Lang;
use super::pick::{INDENT, MEASURE_MARGIN, MEASURE_MAX};

/// 时长只写最高的一个单位。
pub fn duration(ms: u64) -> String {
    super::quantities::largest_unit_text(ms)
}

/// token 数：一千以上写成 `1.2k`。
pub fn tokens(count: u64) -> String {
    super::quantities::thousands(count)
}

fn turns(lang: Lang, count: u32) -> String {
    match lang {
        Lang::Zh => format!("{count} 轮"),
        Lang::En if count == 1 => "1 turn".to_owned(),
        Lang::En => format!("{count} turns"),
    }
}

fn token_count(lang: Lang, count: u64) -> String {
    match lang {
        Lang::Zh => format!("{} token", tokens(count)),
        Lang::En => format!("{} tokens", tokens(count)),
    }
}

/// 达成 / 不可能时括号里的统计：时长 · 轮数 · token。
fn summary(status: &GoalStatus, lang: Lang) -> Vec<String> {
    let mut parts = Vec::new();
    parts.extend(status.duration_ms.map(duration));
    parts.extend(status.iterations.map(|n| turns(lang, n)));
    parts.extend(status.tokens.map(|n| token_count(lang, n)));
    parts
}

/// 量到正文宽度（`min(宽度 − 2, 99)`）的元素按 `width` 排成行。
fn lay(element: Element, width: usize) -> Vec<StyledLine> {
    Layout::lay(&measure(element, MEASURE_MAX, MEASURE_MARGIN), width.max(1))
}

/// 对话里的 `goal_status`（设定 / 清除的标记不显示，调用方已过滤）：Puffin 的检查行（见模块文档）。
pub fn status_lines(status: &GoalStatus, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let zh = ctx.lang == Lang::Zh;
    let theme = &ctx.ink.theme;
    let (label, style, mark) = if status.failed {
        let label = if zh {
            "目标无法达成"
        } else {
            "Goal could not be achieved"
        };
        (
            label,
            theme.fg2.with_attr(attr::BOLD),
            Span::new("✗", Style::fg(theme.err)),
        )
    } else if status.met {
        let label = if zh {
            "目标已达成"
        } else {
            "Goal achieved"
        };
        (
            label,
            theme.fg2.with_attr(attr::BOLD),
            Span::new("✓", Style::fg(theme.ok)),
        )
    } else {
        let label = if zh {
            "目标尚未达成……继续"
        } else {
            "Goal not yet met\u{2026} continuing"
        };
        (label, theme.fg3, Span::new("◎", Style::fg(theme.brand)))
    };
    let mut spans = vec![
        Span::plain(" "),
        mark,
        Span::plain(" "),
        Span::new(label.to_owned(), style),
    ];
    let parts = if status.met || status.failed {
        summary(status, ctx.lang)
    } else {
        Vec::new()
    };
    if !parts.is_empty() {
        let joined = parts.join(" \u{b7} ");
        let text = if zh {
            format!("（{joined}）")
        } else {
            format!(" ({joined})")
        };
        spans.push(Span::new(text, theme.fg3));
    }
    let mut rows = vec![single_line(spans, false)];
    if let (true, Some(reason)) = (status.failed, &status.reason) {
        // 理由在第 3 列的竖线后面（Puffin「行下面」）。
        let rail = vec![
            Span::plain(INDENT),
            Span::new("│", theme.line),
            Span::plain(" "),
        ];
        rows.push(wrap_styled(
            rail.clone(),
            rail,
            vec![Span::new(reason.replace('\r', ""), theme.fg2)],
            Wrap::Word,
        ));
    }
    lay(stack(rows), ctx.width)
}

#[cfg(test)]
#[path = "goal_tests.rs"]
mod tests;
