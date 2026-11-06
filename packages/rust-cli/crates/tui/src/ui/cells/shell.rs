//! bash 模式的输入行与输出行（docs/specs/rust-cli/tui-input.md §2.1）：输入是板上的 `! ` 加命令；输出挂在它下面的竖线上，
//! 结束时从 fg3 渐变到 fg2（stderr 同色）。
//!
//! 样子照 Puffin 的 `!` 命令的块（docs/puffin-tui.md §4.2「Shell 命令（!）」，`shell/shell_view.rs`）：第 1 列琥珀粗体的 `!`，
//! 命令从第 3 列起、每行截断；输出在 `   │ ` 后面，每行一行、放不下时截断（结束后的输出 fg2）。收起时留最新的 10 行，
//! 更早的在它们上面数出来：`… N earlier lines`（Puffin `shell_block::output_rows`；以前留开头 10 行、下面写 `… +N lines`）。
//! 命令的结果（退出码、用时）旧界面的视图没有，不画。
use rust_cli_tui_kit::app::element::{measure, single_line, stack};
use rust_cli_tui_kit::app::widgets::shell_block::TAIL;
use rust_cli_tui_kit::app::widgets::{fade, tools};
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::look::{self, MEASURE_MARGIN, MEASURE_MAX};
use super::{Ctx, detail_text, messages, words};

/// 输入行：板上的命令（[`messages::shell_command`]）。
pub fn input(command: &str, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    messages::shell_command(command, ctx)
}

/// 输出行。`age` 是输出到达（命令结束）以来的 tick：在 [`fade::TICKS`] 里从 fg3 渐变到 fg2（Puffin
/// `shell_block::output_rows`，结束后的输出是你要看的，不退到 quiet）；落定的是 [`super::settled`]。
/// stderr 与 stdout 同色（Puffin：错误输出不用红色，非零退出由结果说）。以前 stderr 是红色、输出直接是 fg2。
pub fn output(stdout: &str, stderr: &str, age: i64, ctx: &Ctx<'_>) -> Vec<StyledLine> {
    let (stdout, stderr) = (stdout.trim_end(), stderr.trim_end());
    let theme = &ctx.ink.theme;
    if stdout.is_empty() && stderr.is_empty() {
        return detail_text(words::no_output(ctx.lang), theme.fg3, ctx);
    }
    let ink = fade::ink(theme, theme.fg3.fg_color(), theme.fg2, age, fade::TICKS);
    // stdout 在前、stderr 在后，按行（Puffin 的块是一份输出，按 `lines_iter` 分行）。
    let lines: Vec<&str> = [stdout, stderr]
        .into_iter()
        .filter(|t| !t.is_empty())
        .flat_map(tools::text::lines_iter)
        .collect();
    let shown = if ctx.expanded {
        &lines[..]
    } else {
        &lines[lines.len().saturating_sub(TAIL)..]
    };
    let hidden = lines.len() - shown.len();
    let railed = |span: Span| {
        let mut spans = look::rail(ctx.ink);
        spans.push(span);
        single_line(spans, false)
    };
    let mut rows = Vec::new();
    if hidden > 0 {
        rows.push(railed(Span::new(
            words::earlier_lines(ctx.lang, hidden),
            theme.fg3,
        )));
    }
    rows.extend(
        shown
            .iter()
            .map(|l| railed(Span::new(l.replace('\t', "    "), ink))),
    );
    look::lay(
        &measure(stack(rows), MEASURE_MAX, MEASURE_MARGIN),
        ctx.width,
    )
}

#[cfg(test)]
#[path = "shell_tests.rs"]
mod tests;
