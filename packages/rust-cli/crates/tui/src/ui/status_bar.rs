//! 状态栏（docs/specs/rust-cli/tui-kit.md §4.2「footer」一行；照 docs/puffin-tui.md §4.2「状态栏」）：左边说谁、在哪——
//! 权限模式与 line 墨色的 `⇧⇥`、模型与档位、cwd、就绪的远端机器；右边说多少——上下文的表、数与量，模型调用的速度。按键
//! 不在这里，在输入行的槽里（`input_box.rs`、`footer.rs` 的 [`super::footer::slot`]）。
//!
//! 排版、部分与数字的写法都是 kit 的（`widgets::status_bar` 的 `rows` / `fit` / `left`，`widgets::stats` 的 `parts`），与
//! Puffin 同一份代码；这里只把旧界面的数据交给它们，模式的名字按语言写（kit 只有英文）。以前这里是 Puffin 规则的一份移植。
//!
//! 旧界面多出的一样（tui-kit.md §4.3「footer 的附加项」）：上下文快满的提示，作为右边的部分（后台命令的 pill 2026-10-31
//! 并进任务行，tui-tasks.md §4）
//! 排在多少之后；旧界面没有 git 分支的数据，不显示分支（§6 登记）。
use rust_cli_tui_kit::app::widgets::stats;
use rust_cli_tui_kit::app::widgets::status_bar::{self as kit, Left};
use rust_cli_tui_kit::render::{Span, Style, StyledLine};
use rust_cli_tui_kit::theme::theme::Theme;

pub use rust_cli_tui_kit::app::widgets::stats::Window;
pub use rust_cli_tui_kit::app::widgets::status_bar::{Part, rows};

/// 模式的部分与挨着它的 `⇧⇥`（kit `left` 的 `mode` 与 `tab`，名字按语言写）。`mode` 是（文字，样式）的几段，模式变了之后
/// `age` 个 tick，从底色淡入（kit `fade::parts`，Puffin `mode_at`；ages 见 status_motion.rs）。
pub fn mode_parts(theme: &Theme, mode: Vec<(String, Style)>, age: i64, tab: bool) -> Vec<Part> {
    let spans = rust_cli_tui_kit::app::widgets::fade::parts(theme, mode, age, None);
    let mut out = vec![Part::new("mode", spans, 100)];
    if tab {
        out.push(Part {
            glue: Some(Vec::new()),
            extra: true,
            after: Some("mode".to_owned()),
            ..Part::new(
                "tab",
                vec![Span::new(" ", Style::DEFAULT), Span::new("⇧⇥", theme.line)],
                10,
            )
        });
    }
    out
}

/// 模式后面目标的记号与字（kit `left` 的 `goal` 与 `goalWords`；记号与字是 [`super::app::App::goal_status_parts`]）；目标出现
/// 或换了状态之后 `age` 个 tick（Puffin `goal_at`）。
pub fn goal_parts(theme: &Theme, goal: Option<(kit::Inked, kit::Inked)>, age: i64) -> Vec<Part> {
    let Some((mark, words)) = goal else {
        return Vec::new();
    };
    kit::left(
        theme,
        &Left {
            goal: Some((mark, words, age)),
            ..Left::default()
        },
    )
    .into_iter()
    .filter(|part| part.key.starts_with("goal"))
    .collect()
}

/// 模式之后的左边（kit `left`）：模型与档位（有时，`/model` 选了之后 `i64` 个 tick，Puffin `label_at`）、cwd、就绪的远端
/// 机器（`@dev-a +1`，kit `machine_rows::where_`）。
pub fn rest(
    theme: &Theme,
    model: Option<(&str, &str, i64)>,
    cwd: &str,
    machines: Option<&str>,
) -> Vec<Part> {
    kit::left(
        theme,
        &Left {
            model,
            cwd,
            machines,
            ..Left::default()
        },
    )
}

/// shell 模式的左边（kit `shell_left`）：进模式之后 `age` 个 tick，同 `!` 一起淡入（Puffin `shell_at`）。
pub fn shell_left(theme: &Theme, age: i64, cwd: &str) -> Vec<Part> {
    kit::shell_left(theme, age, cwd)
}

/// 「多少」的部分：表、数、用掉的数与窗口的大小，然后是速度（kit `stats::parts`）。`context_age`：上下文的数变了之后的
/// tick（Puffin `figures_at["context"]`）；`speed`：kit `LiveSpeed::spans` 画的速度（status_motion.rs），为空时不显示。
pub fn group(
    theme: &Theme,
    context: Option<Window>,
    context_age: i64,
    speed: Vec<Span>,
) -> Vec<Part> {
    stats::parts(
        theme,
        context.as_ref().filter(|c| c.max > 0),
        &|key| {
            if key == "context" {
                context_age
            } else {
                i64::MAX / 4
            }
        },
        speed,
    )
}

/// 排好的状态栏：每行一条 kit 的行（`rows` 的结果）。
pub fn lines(
    theme: &Theme,
    left: &[Part],
    group: &[Part],
    note: &[Part],
    width: usize,
    height: usize,
) -> Vec<StyledLine> {
    rows(left, group, note, width, height, theme.line)
        .into_iter()
        .map(StyledLine::new)
        .collect()
}

#[cfg(test)]
#[path = "status_bar_tests.rs"]
mod tests;
