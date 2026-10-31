//! 定时任务的提示文本（docs/specs/rust-cli/tools-more.md §16.4、§17.2）。
//!
//! 文字逐字固定：错过任务的提示里产品名按 cron 文字的决定写成 ZCode、路径写成 `.zcode`；ScheduleWakeup 的
//! 结果按 brief 模式关、不预先报告状态的写法。

use rust_cli_base::names::{DOT_DIR, SCHEDULED_TASKS_FILE};
use rust_cli_kernel_api::cron_store::StoredCronTask;

use crate::cron_expr;

/// JS `toLocaleString('en-US')` 的写法：`10/18/2026, 1:02:03 AM`（Bun 1.x 时间与 AM/PM 之间是普通空格）。
pub(crate) fn locale_string(utc_ms: u64, offset: &dyn Fn(u64) -> i32) -> String {
    let (year, month, day, hour, minute, second) = cron_expr::local_parts(utc_ms, offset);
    let (twelve, half) = cron_expr::twelve_hour(hour);
    format!("{month}/{day}/{year}, {twelve}:{minute:02}:{second:02} {half}")
}

/// JS `toTimeString().slice(0, 8)`：本地 `HH:MM:SS`。
pub(crate) fn clock_time(utc_ms: u64, offset: &dyn Fn(u64) -> i32) -> String {
    let (_, _, _, hour, minute, second) = cron_expr::local_parts(utc_ms, offset);
    format!("{hour:02}:{minute:02}:{second:02}")
}

/// 用比正文里最长的反引号串多一个（至少 3 个）的围栏包起来。
fn fence(text: &str) -> String {
    const MIN_MARKS: usize = 3;
    let longest = text
        .split(|character| character != '`')
        .map(str::len)
        .max()
        .unwrap_or(0);
    let marks = "`".repeat((longest + 1).max(MIN_MARKS));
    format!("{marks}\n{text}\n{marks}")
}

/// 错过的一次性任务的提示。
pub(crate) fn missed(tasks: &[StoredCronTask], offset: &dyn Fn(u64) -> i32) -> String {
    let plural = tasks.len() > 1;
    let pick = |many: &'static str, one: &'static str| if plural { many } else { one };
    let header = format!(
        "The following one-shot scheduled task{} missed while ZCode was not running. {} already been removed from {DOT_DIR}/{SCHEDULED_TASKS_FILE}.\n\nDo NOT execute {} yet. First use the AskUserQuestion tool to ask whether to run {} now. Only execute if the user confirms.",
        pick("s were", " was"),
        pick("They have", "It has"),
        pick("these prompts", "this prompt"),
        pick("each one", "it"),
    );
    let blocks: Vec<String> = tasks
        .iter()
        .map(|task| {
            format!(
                "[{}, created {}]\n{}",
                cron_expr::human(&task.cron),
                locale_string(task.created_at, offset),
                fence(&task.prompt)
            )
        })
        .collect();
    format!("{header}\n\n{}", blocks.join("\n\n"))
}

/// ScheduleWakeup 结果的收尾句（brief 模式关时的写法）。
const END_TURN: &str = "Then write the loop's outcome for the user as ordinary visible response text \u{2014} and end the turn.";

/// 排上了：`Next wakeup scheduled for …`（不预先报告状态时的写法）。
pub(crate) fn wakeup_scheduled(at: &str, in_seconds: u64, clamped: Option<i64>) -> String {
    let note = clamped
        .map(|seconds| format!(" (clamped to {seconds}s from your requested value)"))
        .unwrap_or_default();
    format!(
        "Next wakeup scheduled for {at} (in {in_seconds}s){note}. If you owe the user a status update this tick, write it now as ordinary response text; then end the turn \u{2014} the harness re-invokes you when the wakeup fires or a task-notification arrives."
    )
}

/// 链满 7 天，没有排。
pub(crate) fn wakeup_ended() -> String {
    format!(
        "Wakeup not scheduled. The loop reached its maximum duration \u{2014} the loop has ended; do not re-issue. {END_TURN}"
    )
}

/// `stop: true` 的结果。
pub(crate) fn loop_stopped(cancelled: usize) -> String {
    let monitor = format!("If you armed a Monitor for this loop, TaskStop it now. {END_TURN}");
    if cancelled == 0 {
        return format!(
            "Loop stopped \u{2014} any dynamic loop in this session is ended; there was no pending wakeup to cancel. If you are running a fixed-interval /loop (a recurring cron), it is NOT stopped by this call \u{2014} cancel it with CronDelete. {monitor}"
        );
    }
    format!(
        "Loop stopped \u{2014} cancelled {cancelled} pending wakeup(s); no further dynamic-loop wakeups scheduled. {monitor}"
    )
}

#[cfg(test)]
#[path = "cron_texts_tests.rs"]
mod tests;
