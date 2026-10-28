// 改写自 apps/zcode-host/crates/tui/src/ui/bottom.rs（feat/rs 79b6691c55）：排队的消息来自视图；`@` 文件列表按
// tui-input.md §1.2 重写。
//! 底部区域的两部分（docs/specs/rust-tui.md §8.3、§8.4）：排队的消息（含下面那一行立即发送的键，
//! docs/specs/rust-cli/tui.md §5.1.1）、斜杠命令与 `@` 文件的弹出列表。
//! 状态行见 `status_line.rs`，输入框见 `input_box.rs`，footer 与快捷键帮助见 `footer.rs`。
use std::time::Duration;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthStr;

use super::app::App;
use super::commands;
use super::composer::Popup;
use super::text::{truncate_line, wrap_text};

fn dim() -> Style {
    Style::default().add_modifier(Modifier::DIM)
}

/// 排队的消息：每条 dim 的 `❯ 文字`，续行缩进 2 列，最多 3 行（超出时第 3 行末尾写 `…`）。
pub fn queue(app: &App, width: usize) -> Vec<Line<'static>> {
    const LIMIT: usize = 3;
    let mut lines = Vec::new();
    let queued = app.view.queue().iter().map(|item| item.text.clone());
    // 新会话创建期间留下的输入排在后面（tui.md §4.1）。
    let held = app.held.iter().filter_map(held_text);
    // 等回合结束的 `/clear`、`/resume`（修复线 F38）排在最后。
    let after_turn = app.after_turn.iter().map(|(label, _)| label.clone());
    for text in queued.chain(held).chain(after_turn) {
        // 排队的 bash 模式输入写成 `! 命令`（tui-input.md §2.1）。
        let shown = rust_cli_kernel_api::user_input::display(&text);
        let text = shown.as_deref().unwrap_or(&text);
        let mut item = wrap_text(
            text.trim(),
            dim(),
            width.saturating_sub(2),
            &[Span::styled("❯ ", dim())],
            &[Span::raw("  ")],
        );
        if item.len() > LIMIT {
            item.truncate(LIMIT);
            if let Some(last) = item.last_mut() {
                last.spans.push(Span::styled(" …", dim()));
            }
        }
        lines.extend(item);
    }
    // 立即发送的键（修复线 F104，tui.md §5.1.1）：队列里有插话、回合在跑时紧跟在全部队列项之后（暂存的提示之前）。
    // 整行一种次要色、键不加粗（修复线 F105，用户要求整行灰；原来按按键提示组件把键画成 `key` 样式）。
    if let Some(hint) = app.send_now_hint() {
        lines.push(Line::from(Span::styled(
            format!("  {hint}"),
            app.palette.secondary(),
        )));
    }
    // ctrl+s 暂存后的提示（tui-input.md §5）。
    if app.composer.has_stash() {
        lines.push(Line::from(Span::styled(
            format!("  {}", super::i18n_input::stashed(app.lang)),
            dim(),
        )));
    }
    lines
}

/// 留下的输入命令要显示的文字（bash 模式写成 `! 命令`）。
fn held_text(command: &rust_cli_kernel_api::session_api::Command) -> Option<String> {
    use rust_cli_kernel_api::message::ContentBlock;
    use rust_cli_kernel_api::session_api::Command;
    match command {
        Command::Submit { text, .. } => Some(text.clone()),
        Command::Bash { command, .. } => Some(format!("! {command}")),
        Command::SubmitContent { content, .. } => Some(
            content
                .iter()
                .filter_map(|block| match block {
                    ContentBlock::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("\n"),
        ),
        _ => None,
    }
}

/// 弹出列表的行数：max(1, min(max(6, 高度 / 2), 高度 − 3))；不足时用空行补齐，框不跳动。
pub fn popup_rows(height: usize) -> usize {
    (height / 2).max(6).min(height.saturating_sub(3)).max(1)
}

fn empty_row(text: &str) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), dim()))
}

/// 一行条目，按选择列表组件画（docs/specs/rust-cli/tui.md §7.4，修复线 F26）：当前行 `› ` 加底色条、名字粗体；
/// 其余行名字正常、说明灰色（原来是 `❯ ` 加强调色、其余行整行 dim）。`matched` 是名称里匹配到的字符下标（粗体）。
fn item(
    app: &App,
    chosen: bool,
    name: &str,
    matched: &[usize],
    name_width: usize,
    description: &str,
    width: usize,
) -> Line<'static> {
    let mut spans = vec![super::pick::pointer(chosen)];
    for (index, c) in name.chars().enumerate() {
        let style = if matched.contains(&index) {
            Style::default().add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(c.to_string(), style));
    }
    if !description.is_empty() {
        spans.push(Span::raw(
            " ".repeat(name_width.saturating_sub(name.width())),
        ));
        spans.push(Span::styled(
            description.to_owned(),
            app.palette.secondary(),
        ));
    }
    super::pick::paint(
        truncate_line(Line::from(spans), width),
        chosen,
        &app.palette,
        width,
    )
}

/// 斜杠命令或文件的弹出列表（替换 footer，行数固定）；返回行与第一行对应的条目下标（没有条目时为
/// None）。
pub fn popup(app: &App, width: usize, height: usize) -> (Vec<Line<'static>>, Option<usize>) {
    let rows = popup_rows(height);
    let (mut lines, first) = match &app.composer.popup {
        Some(Popup::Slash { selected }) => {
            let typed = app.composer.area.text().trim_start_matches('/').to_owned();
            let matches = app.composer.slash_matches();
            if matches.is_empty() {
                (vec![empty_row(app.strings.no_matches)], None)
            } else {
                // 命令列补到最长名称 + 5，最多宽度的 40%。
                let name_width = matches
                    .iter()
                    .map(|c| c.name().width() + 1 + 5)
                    .max()
                    .unwrap_or(0)
                    .min(width * 2 / 5);
                let start = selected.saturating_sub(rows - 1);
                let lines = matches
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(rows)
                    .map(|(index, command)| {
                        let name = format!("/{}", command.name());
                        let matched: Vec<usize> =
                            commands::matched_positions(command.name(), &typed)
                                .into_iter()
                                .map(|i| i + 1)
                                .collect();
                        let description = command.description(app.lang);
                        item(
                            app,
                            index == *selected,
                            &name,
                            &matched,
                            name_width,
                            description,
                            width,
                        )
                    })
                    .collect();
                (lines, Some(start))
            }
        }
        Some(Popup::Files { selected }) => {
            let matches = &app.composer.files.matches;
            let start = selected.saturating_sub(rows - 1);
            let lines = matches
                .iter()
                .enumerate()
                .skip(start)
                .take(rows)
                .map(|(index, path)| item(app, index == *selected, path, &[], 0, "", width))
                .collect();
            (lines, Some(start))
        }
        None => (Vec::new(), None),
    };
    if !lines.is_empty() {
        lines.resize(rows, Line::default());
    }
    (lines, first)
}

/// 动画的刷新间隔：扫光约 30 fps。
pub const ANIMATION: Duration = Duration::from_millis(33);

#[cfg(test)]
#[path = "bottom_tests.rs"]
mod tests;
