// 改写自 apps/zcode-host/crates/tui/src/ui/bottom.rs（feat/rs 79b6691c55）：排队的消息来自视图；`@` 文件列表按
// tui-input.md §1.2 重写。
//! 输入框的板上、输入行之上的几层（docs/specs/rust-tui.md §8.3、§8.4；样子照 Puffin，docs/specs/rust-cli/tui-kit.md §4.2）：
//!
//! - 排队的消息：Puffin 的栈（docs/puffin-tui.md §4.2「发出的消息」的「栈」，`shell/composer.rs` 的 `stack_rows`）——每条
//!   一行，第 1 列 fg3 的 `›`、第 3 列起 fg2 的文字，换行画成空格，放不下时 `…`；至多四行，再多时最上面一行是 fg3 的
//!   `N earlier`；全部都等下一轮时最上面一行的槽里写 fg3 的 `next turn`。下面是立即发送的键与暂存的提示（fg3，第 3 列起；tui-kit.md §4.3）。
//! - 斜杠命令：Puffin 的命令架（§4.2「命令面板（shelf）」，`shell/composer.rs` 的 `shelf_rows`）——第 3 列起 `/名字` 补齐，
//!   空三格接描述（去掉末尾句号）；选中的名字 brand、描述 fg2，其余名字 fg2、描述 fg3；窗口跟着选中的行，藏起来的用
//!   `↑ n more` / `↓ n more` 代替最上或最下的一行。条目是旧界面的命令表与 skill（tui.md §6）。
//! - `@` 文件：Puffin 的 `@` 面板（§4.2「提及（@）」）——组、行与宽终端右边的预览都由 kit 的 `at_panel::rows` 画，组由
//!   `at_view.rs` 建。以前这里画它的窄版、没有预览。
//!
//! 画法：kit 的 [`Element`]，墨色经 `Ink` 的 token。
use std::time::Duration;

use rust_cli_kernel_api::session_api::Delivery;
use rust_cli_tui_kit::app::element::{Element, single_line};
use rust_cli_tui_kit::app::widgets::key_hints::{edge, on_row};
use rust_cli_tui_kit::render::width::string_width;
use rust_cli_tui_kit::render::{Span, Style};
use rust_cli_tui_kit::theme::theme::Theme;

use super::app::App;
use super::composer::Popup;
use super::lines::truncate;

/// 正文从第 3 列起。
const INDENT: &str = "   ";
/// 栈至多这么多行（Puffin `STACK_ROWS`）。
pub const STACK_ROWS: usize = 4;

// ---- 栈 ----

/// 栈的一行（Puffin `stack_rows`）：第 1 列 fg3 的 `›`，第 3 列起 fg2 的文字（一行，换行画成空格，放不下时 `…`）。`label`
/// 是最上面一行在槽的位置写的说明（fg3）；有它时文字给它与它前面的两格让出地方。
pub fn stack_row(theme: &Theme, text: &str, width: usize, label: Option<&str>) -> Element {
    let room = match label {
        Some(label) => edge(width).saturating_sub(2 + string_width(label)),
        None => width.saturating_sub(1),
    };
    let words = truncate(
        &text.replace('\n', " "),
        room.saturating_sub(INDENT.len()).max(1),
    );
    let row = single_line(
        vec![
            Span::new(" ", Style::DEFAULT),
            Span::new("›", theme.fg3),
            Span::new(" ", Style::DEFAULT),
            Span::new(words, theme.fg2),
        ],
        false,
    );
    match label {
        Some(label) => on_row(row, vec![Span::new(label.to_owned(), theme.fg3)]),
        None => row,
    }
}

/// 排队的消息（栈）与它下面的提示。
pub fn queue(app: &App, width: usize) -> Vec<Element> {
    let theme = &app.ink.theme;
    // 栈只放等下一轮的（Puffin ⇥ 推上栈的消息，tui-kit.md §4.0「板」）：插进这一回合的（Delivery::Steer）与新会话创建期间
    // 留下的输入在 transcript 的尾巴里（`transcript_tail.rs`）。以前它们也在这里。
    // 还没认领的输入也在这里（`view/in_flight.rs`）：按键的那一帧就上栈，与内核排上之后同一行。
    let outgoing = app.view.outgoing();
    let queued = outgoing
        .iter()
        .filter(|item| item.delivery == Delivery::Queue)
        .map(|item| item.text.clone());
    // 等回合结束的 `/clear`、`/resume`（修复线 F38）排在最后。
    let after_turn = app.after_turn.iter().map(|(label, _)| label.clone());
    let next_turn = true;
    let items: Vec<String> = queued
        .chain(after_turn)
        .map(|text| {
            // 排队的 bash 模式输入写成 `! 命令`（tui-input.md §2.1）。
            let shown = rust_cli_kernel_api::user_input::display(&text);
            // 提及写回打出来的样子（会话的链接是 `@标题`，Puffin `stack_rows` 的 `unlinked`；kit `mention_wire`）。
            let shown = shown.unwrap_or(text);
            rust_cli_tui_kit::app::widgets::mention_wire::unlinked(&shown)
                .trim()
                .to_owned()
        })
        .collect();
    let mut rows = Vec::new();
    let shown = if items.len() > STACK_ROWS {
        &items[items.len() - (STACK_ROWS - 1)..]
    } else {
        &items[..]
    };
    let earlier = items.len() - shown.len();
    let label = next_turn.then(|| super::i18n_input::next_turn(app.lang));
    if earlier > 0 {
        let row = single_line(
            vec![
                Span::new(INDENT, Style::DEFAULT),
                Span::new(super::i18n_input::earlier(app.lang, earlier), theme.fg3),
            ],
            false,
        );
        rows.push(match label {
            Some(label) => on_row(row, vec![Span::new(label, theme.fg3)]),
            None => row,
        });
    }
    rows.extend(shown.iter().enumerate().map(|(i, text)| {
        let top = earlier == 0 && i == 0;
        stack_row(theme, text, width, label.filter(|_| top))
    }));
    // ctrl+s 暂存后的提示（tui-input.md §5）。
    if app.composer.has_stash() {
        rows.push(single_line(
            vec![
                Span::new(INDENT, Style::DEFAULT),
                Span::new(super::i18n_input::stashed(app.lang), theme.fg3),
            ],
            false,
        ));
    }
    rows
}

/// 留下的输入命令要显示的文字（bash 模式写成 `! 命令`）。
pub(crate) fn held_text(command: &rust_cli_kernel_api::session_api::Command) -> Option<String> {
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

// ---- 命令架与 `@` 面板 ----

/// 斜杠命令或文件的面板（在板上、输入行之上）；返回行与每一行对应的条目下标（不可点的行为 None）。
pub fn popup(app: &App, width: usize) -> Vec<(Element, Option<usize>)> {
    match &app.composer.popup {
        // 命令架的行在 `shelf_rows.rs`（kit 的 `widgets::shelf`，tui-bottom）。
        Some(Popup::Slash { selected }) => super::shelf_rows::rows(app, *selected),
        // `@` 面板：Puffin 的组、行与右边的预览（kit `at_panel::rows`，`at_view.rs`）。
        Some(Popup::Files { .. }) => app.at_lines(width, app.status_motion.tick(), 0),
        // `$` 面板：kit `skill_panel::rows`（`skill_view.rs`），行不可点。
        Some(Popup::Skills { .. }) => app
            .skill_lines(width, app.status_motion.tick())
            .into_iter()
            .map(|row| (row, None))
            .collect(),
        // 光标在粘贴的帽上：它的预览（`input_box::paste_peek`，tui-bottom）。
        None => super::input_box::paste_peek(app, width),
    }
}

/// 动画的刷新间隔：扫光约 30 fps。
pub const ANIMATION: Duration = Duration::from_millis(33);

#[cfg(test)]
#[path = "bottom_tests.rs"]
mod tests;
