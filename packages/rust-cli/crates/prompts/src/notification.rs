//! 后台命令与 Monitor 的终态通知（`<task-notification>`，docs/specs/rust-cli/query.md §5、tools-more.md §19.6）。
//!
//! 入参是基本类型：调用方（会话 actor、子代理）从执行端口的通知取，状态与“被停掉”按 kernel-api 的
//! `ShellNotice::status`、`ShellNotice::stopped`。工作线 F131 从 query 的 `notification.rs` 移来（query 自己不用；
//! agents 以前经 runtime 注入的两个函数指针拿同样的文字），文字逐字不变。

use crate::monitor::{self, Ending};

/// 一条终态通知要写的内容（定义在 base，kernel-api 的 `ShellNotice::task_notice` 由执行端口的通知换算，工作线 F134）。
pub use rust_cli_base::notice::TaskNotice;

/// 终态通知的全文：说明加信封，`<system-reminder>` 包装。说明与包装与子代理的通知同一份（`notification_preamble.txt`）。
pub fn task_notification(notice: &TaskNotice<'_>) -> String {
    crate::agents::notification_wrap(&task_envelope(notice))
}

/// 只有 `<task-notification>` 信封（不带说明与包装）：子代理收到的命令通知由 agents 与它派生的子代理的
/// 通知一起包装（docs/specs/rust-cli/agents.md §5.2，L18）。
pub fn task_envelope(notice: &TaskNotice<'_>) -> String {
    // Monitor 的终态文字不同（工作线 F83）；会话没有在看它时（例如会话重新打开后才送达）没有剩下的行。
    if notice.monitor {
        return monitor_envelope(notice, None);
    }
    let description = notice.description;
    let summary = match (notice.stopped, notice.exit_code) {
        (true, _) => format!("Background command \"{description}\" was stopped"),
        (false, Some(0)) => {
            format!("Background command \"{description}\" completed (exit code 0)")
        }
        (false, Some(code)) => {
            format!("Background command \"{description}\" failed with exit code {code}")
        }
        (false, None) => format!("Background command \"{description}\" failed"),
    };
    let tool_use = notice
        .tool_use
        .map(|id| format!("\n<tool-use-id>{id}</tool-use-id>"))
        .unwrap_or_default();
    format!(
        "<task-notification>\n<task-id>{}</task-id>{tool_use}\n<output-file>{}</output-file>\n<status>{}</status>\n<summary>{summary}</summary>\n</task-notification>",
        notice.task, notice.output, notice.status
    )
}

/// Monitor 的终态通知的信封（docs/specs/rust-cli/tools-more.md §19.6）：summary 按结局写，说明与
/// 剩下的行都转义；`events` 是拿到令牌的剩下的行，放进 `<event>`。
pub fn monitor_envelope(notice: &TaskNotice<'_>, events: Option<&str>) -> String {
    let description = monitor::escape(notice.description);
    let ending = match notice.status {
        "completed" => Ending::Completed {
            output: notice.output_bytes > 0,
        },
        "failed" => Ending::Failed,
        _ => Ending::Stopped,
    };
    let summary = monitor::terminal_summary(&description, ending, notice.exit_code);
    let tool_use = notice
        .tool_use
        .map(|id| format!("\n<tool-use-id>{}</tool-use-id>", monitor::escape(id)))
        .unwrap_or_default();
    let events = events
        .map(|lines| format!("\n<event>{}</event>", monitor::escape(lines)))
        .unwrap_or_default();
    format!(
        "<task-notification>\n<task-id>{}</task-id>{tool_use}\n<output-file>{}</output-file>\n<status>{}</status>\n<summary>{summary}</summary>{events}\n</task-notification>",
        monitor::escape(notice.task),
        monitor::escape(notice.output),
        notice.status,
    )
}

#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;
