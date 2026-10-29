// 改写自 apps/zcode-host/crates/tui/src/ui/input_box.rs（feat/rs 79b6691c55）：状态来自 `view::Transcript`。
//! 输入框（docs/specs/rust-tui.md §8.4）：上下各一条 dim 的 `─` 线；上边线右侧是会话标题，浏览输入
//! 历史时左侧是 `History k/n`。提示符 `❯ `（运行中 dim），续行缩进 2 列；行数超出时内部滚动，右侧
//! 显示 `↑` / `↓`。查看子代理时边线换成子代理色，标题换成子代理的描述（修复线 F62，docs/specs/rust-cli/tui-panels.md
//! §1.6）。
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::app::App;
use super::i18n::Lang;
use super::text::truncate;

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// 新会话的占位例子：按工作区固定选一个。
fn example(workspace: &str, lang: Lang) -> &'static str {
    const EN: [&str; 4] = [
        "explain this codebase",
        "fix lint errors",
        "write a test for the last change",
        "what changed recently?",
    ];
    const ZH: [&str; 4] = [
        "解释这个代码库",
        "修复 lint 错误",
        "为最近的改动写测试",
        "最近改了什么？",
    ];
    let index = workspace.bytes().map(usize::from).sum::<usize>() % EN.len();
    match lang {
        Lang::En => EN[index],
        Lang::Zh => ZH[index],
    }
}

/// 占位文字里子代理类型的最大列数（tui-panels.md §1.6）。
const VIEWING_NAME_MAX: usize = 20;

/// 输入框为空时的占位文字。
fn placeholder(app: &App) -> String {
    if let Some(viewing) = &app.viewing {
        let kind = truncate(viewing.type_name(), VIEWING_NAME_MAX);
        return super::i18n_panels::viewing_placeholder(app.lang, &kind);
    }
    let queued = !app.view.queue().is_empty();
    let fresh = !app.view.has_messages();
    match app.lang {
        _ if app.creating => app.strings.starting_session.to_owned(),
        _ if app.composer.mode == super::composer::InputMode::Bash => {
            super::i18n_input::bash_placeholder(app.lang).to_owned()
        }
        _ if queued => app.strings.placeholder_queued.to_owned(),
        Lang::En if fresh => format!("Try \"{}\"", example(app.workspace(), app.lang)),
        Lang::Zh if fresh => format!("试试“{}”", example(app.workspace(), app.lang)),
        _ => app.strings.placeholder.to_owned(),
    }
}

/// 边线的样式：查看子代理时用子代理色（修复线 F62，tui-panels.md §1.6；bash 模式与搜索模式的颜色让位），搜索模式用
/// 强调色（修复线 F114，tui-input.md §2.3.5），bash 模式用 bash 色（tui-input.md §2.1），否则 dim。
fn rule(app: &App) -> Style {
    if app.viewing.is_some() {
        app.palette.subagent_rule()
    } else if app.find.is_some() {
        Style::default().fg(app.palette.accent)
    } else if app.composer.mode == super::composer::InputMode::Bash {
        Style::default().fg(app.palette.bash())
    } else {
        dim()
    }
}

/// 上边线：左侧 `History k/n`（浏览历史时），右侧会话标题（后面再画 1 个 `─`）；查看子代理时见 [`viewing_rule`]。
fn top_rule(app: &App, width: usize) -> Line<'static> {
    if let Some(viewing) = &app.viewing {
        return viewing_rule(app, &viewing.description, width);
    }
    let left = app
        .composer
        .history_position()
        .map(|(index, total)| format!("History {index}/{total} "))
        .unwrap_or_default();
    let title = Some(app.view.title().trim())
        .filter(|t| !t.is_empty())
        .map(|t| truncate(t, width / 2))
        .map(|t| format!(" {t} ─"))
        .unwrap_or_default();
    let fill = width.saturating_sub(left.width() + title.width());
    Line::from(Span::styled(
        format!("{left}{}{title}", "─".repeat(fill)),
        rule(app),
    ))
}

/// 查看子代理时的上边线（tui-panels.md §1.6）：右侧 ` <描述> ` 是子代理色的底色，后面 1 个 `─`，左边用 `─` 补满。
/// 描述的空白压成一个空格、截到宽度 − 3 列；宽度不到 4 列或描述为空时只有边线。
fn viewing_rule(app: &App, description: &str, width: usize) -> Line<'static> {
    let style = rule(app);
    let text = description.split_whitespace().collect::<Vec<_>>().join(" ");
    let room = width.saturating_sub(3);
    if text.is_empty() || room == 0 {
        return Line::from(Span::styled("─".repeat(width), style));
    }
    let label = format!(" {} ", truncate(&text, room));
    let fill = width.saturating_sub(label.width() + 1);
    Line::from(vec![
        Span::styled("─".repeat(fill), style),
        Span::styled(label, app.palette.subagent_label()),
        Span::styled("─", style),
    ])
}

/// 一行输入：粘贴的占位文字青色。
fn spans(text: &str, placeholders: &[&str]) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        let next = placeholders
            .iter()
            .filter_map(|p| rest.find(p).map(|at| (at, *p)))
            .min_by_key(|(at, _)| *at);
        let Some((at, placeholder)) = next else {
            spans.push(Span::raw(rest.to_owned()));
            break;
        };
        if at > 0 {
            spans.push(Span::raw(rest[..at].to_owned()));
        }
        spans.push(Span::styled(
            placeholder.to_owned(),
            Style::default().fg(Color::Cyan),
        ));
        rest = &rest[at + placeholder.len()..];
    }
    spans
}

/// 文字区的宽度：屏幕宽度 − 3（前面 2 列是提示符，最后一列留给行尾的光标与 `↑` / `↓`）。绘制、
/// 点击定位与上下移动都用它折行。
pub fn text_width(width: usize) -> usize {
    width.saturating_sub(3)
}

/// 文字区最多的行数：max(3, 屏幕高度 / 2 − 5)。
pub fn max_rows(height: usize) -> usize {
    (height / 2).saturating_sub(5).max(3)
}

/// 输入框的行、光标（相对行，列）与第一可见行。搜索历史时显示匹配的那条历史。
pub fn input_box(
    app: &App,
    width: usize,
    max_rows: usize,
) -> (Vec<Line<'static>>, Option<(u16, u16)>, usize) {
    // 修复：这里原来按宽度 − 2 折行，而点击定位与上下移动按宽度 − 3（`App::composer_width`），
    // 行宽落在两者之间时点击位置和画面错开、行尾的光标落到屏幕外。两边都改用 `text_width`。
    let text_width = text_width(width);
    let mut lines = vec![top_rule(app, width)];
    let area = &app.composer.area;
    let busy = app.running() || app.creating;
    let prompt = if app.composer.mode == super::composer::InputMode::Bash {
        Span::styled("! ", Style::default().fg(app.palette.bash()))
    } else {
        Span::styled("❯ ", if busy { dim() } else { Style::default() })
    };
    let mut cursor = None;
    let mut first_row = 0;
    if let Some(find) = &app.find {
        // 搜索模式（修复线 F114）：`⌕ ` 加查询，右侧是计数；光标在查询末尾。
        let (line, column) = super::find::input_line(app, find, width);
        lines.push(line);
        cursor = Some((1, column));
    } else if let Some((query, _)) = &app.composer.search {
        // 搜索历史：显示匹配到的那条，查询出现的地方用黄色。
        let shown = app.composer.search_match().unwrap_or(area.text());
        let mut line = vec![prompt];
        let mut rest = shown;
        while let Some(at) = (!query.is_empty())
            .then(|| rest.find(query.as_str()))
            .flatten()
        {
            line.push(Span::raw(rest[..at].to_owned()));
            line.push(Span::styled(
                query.clone(),
                Style::default().fg(Color::Yellow),
            ));
            rest = &rest[at + query.len()..];
        }
        line.push(Span::raw(rest.to_owned()));
        lines.push(super::text::truncate_line(Line::from(line), width));
    } else if area.is_empty() {
        // 光标落在占位文字的第一个字符上。
        let line = Line::from(vec![prompt, Span::styled(placeholder(app), dim())]);
        lines.push(super::text::truncate_line(line, width));
        cursor = Some((1, 2));
    } else {
        let placeholders: Vec<&str> = app.composer.placeholders().collect();
        let rows = area.rows(text_width);
        let (cursor_row, cursor_col) = area.cursor_position(text_width);
        let first = rows.len().saturating_sub(max_rows).min(cursor_row);
        first_row = first;
        let shown = rows.len().min(first + max_rows);
        for (index, row) in rows.iter().enumerate().skip(first).take(max_rows) {
            let lead = if index == 0 {
                prompt.clone()
            } else {
                Span::raw("  ")
            };
            let text = area.text()[row.start..row.end].trim_end_matches('\n');
            let mut line = vec![lead];
            line.extend(spans(text, &placeholders));
            let mut line = Line::from(line);
            // 选区与这一行相交的部分反色（列从提示符后面算起）。
            if let Some((start, end)) = area.selection() {
                let (from, to) = (start.max(row.start), end.min(row.start + text.len()));
                if from < to {
                    let column = |at: usize| 2 + area.text()[row.start..at].width();
                    super::selection::reverse(&mut line, column(from), column(to));
                }
            }
            let more = (index == first && first > 0)
                .then_some("↑")
                .or((index + 1 == shown && shown < rows.len()).then_some("↓"));
            if let Some(arrow) = more {
                let used = line.width();
                line.push_span(Span::raw(" ".repeat(width.saturating_sub(used + 1))));
                line.push_span(Span::styled(arrow, dim()));
            }
            lines.push(line);
        }
        // 只输入了带参数的命令名（`/名称 `）：后面用 dim 文字写参数提示。
        let typed = area
            .text()
            .strip_prefix('/')
            .and_then(|rest| rest.strip_suffix(' '));
        // TUI 命令的参数提示；skill 用 frontmatter 的 `argument-hint`（tui.md §6）。
        let hint = typed.and_then(|name| {
            super::commands::find(name)
                .and_then(super::commands::Command::arg_hint)
                .or_else(|| {
                    super::commands::skill(&app.composer.skills, name)
                        .and_then(|skill| skill.argument_hint.as_deref())
                })
        });
        if let (Some(hint), Some(last)) = (hint, lines.last_mut()) {
            last.spans.push(Span::styled(hint.to_owned(), dim()));
        }
        cursor = Some(((cursor_row - first + 1) as u16, (cursor_col + 2) as u16));
    }
    lines.push(Line::from(Span::styled("─".repeat(width), rule(app))));
    (lines, cursor, first_row)
}

#[cfg(test)]
#[path = "input_box_tests.rs"]
mod tests;
