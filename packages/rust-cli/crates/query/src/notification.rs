//! 后台命令终态通知的文本（复制自 tools 的 `bash_output`，L4a 实现；移到这里是因为通知由会话
//! actor 交给主循环，tools 不再需要它）。

use rust_cli_kernel_api::env::Outcome;
use rust_cli_kernel_api::env_shell::{BackgroundKind, ShellNotice, SocketEvent};
use rust_cli_kernel_api::monitor_watch::Housekeeping;
use rust_cli_prompts::monitor::{self, Ending};

/// 通知里的 `<status>`：`completed`、`failed` 或 `killed`（会话的 `BackgroundWork` 事件也用它）。
pub fn task_status(notice: &ShellNotice) -> &'static str {
    match (&notice.outcome, notice.exit_code) {
        (Outcome::Completed, Some(0)) => "completed",
        (Outcome::Completed | Outcome::Failed { .. }, _) => "failed",
        _ => "killed",
    }
}

/// 后台命令终态通知的文本（`<task-notification>`）：
/// 说明加信封，`<system-reminder>` 包装（docs/specs/rust-cli/query.md §5）。说明与包装与子代理的通知
/// 同一份（prompts 的 `notification_preamble.txt`，修复线 F58：以前这里另写了一份同样的文字）。
pub fn task_notification(notice: &ShellNotice) -> String {
    rust_cli_prompts::agents::notification_wrap(&task_envelope(notice))
}

/// 只有 `<task-notification>` 信封（不带说明与包装）：子代理收到的命令通知由 agents 与它派生的子代理的
/// 通知一起包装（docs/specs/rust-cli/agents.md §5.2，L18）。
pub fn task_envelope(notice: &ShellNotice) -> String {
    // Monitor 的终态文字不同（工作线 F83）；actor 没有在看它时（例如会话重新打开后才送达）没有剩下的行。
    if notice.kind == BackgroundKind::Monitor {
        return monitor_envelope(notice, None);
    }
    let description = &notice.description;
    let status = task_status(notice);
    let summary = match (&notice.outcome, notice.exit_code) {
        (Outcome::Completed, Some(0)) => {
            format!("Background command \"{description}\" completed (exit code 0)")
        }
        (Outcome::Completed, Some(code)) => {
            format!("Background command \"{description}\" failed with exit code {code}")
        }
        (Outcome::Completed | Outcome::Failed { .. }, None) => {
            format!("Background command \"{description}\" failed")
        }
        _ => format!("Background command \"{description}\" was stopped"),
    };
    let tool_use = notice
        .tool_use
        .as_ref()
        .map(|id| format!("\n<tool-use-id>{id}</tool-use-id>"))
        .unwrap_or_default();
    format!(
        "<task-notification>\n<task-id>{}</task-id>{tool_use}\n<output-file>{}</output-file>\n<status>{status}</status>\n<summary>{summary}</summary>\n</task-notification>",
        notice.task,
        notice.output.as_str()
    )
}

/// Monitor 的终态通知的信封（docs/specs/rust-cli/tools-more.md §19.6）：summary 按结局写，说明与
/// 剩下的行都转义；`events` 是拿到令牌的剩下的行，放进 `<event>`。
pub fn monitor_envelope(notice: &ShellNotice, events: Option<&str>) -> String {
    let description = monitor::escape(&notice.description);
    let ending = match task_status(notice) {
        "completed" => Ending::Completed {
            output: notice.output_bytes > 0,
        },
        "failed" => Ending::Failed,
        _ => Ending::Stopped,
    };
    let summary = monitor::terminal_summary(&description, ending, notice.exit_code);
    let tool_use = notice
        .tool_use
        .as_ref()
        .map(|id| {
            format!(
                "\n<tool-use-id>{}</tool-use-id>",
                monitor::escape(id.as_str())
            )
        })
        .unwrap_or_default();
    let events = events
        .map(|lines| format!("\n<event>{}</event>", monitor::escape(lines)))
        .unwrap_or_default();
    format!(
        "<task-notification>\n<task-id>{}</task-id>{tool_use}\n<output-file>{}</output-file>\n<status>{}</status>\n<summary>{summary}</summary>{events}\n</task-notification>",
        monitor::escape(notice.task.as_str()),
        monitor::escape(notice.output.as_str()),
        task_status(notice),
    )
}

/// Monitor 的说明的文字（结构化的种类换成 prompts 的原文）。
pub fn monitor_notice_text(notice: &Housekeeping) -> String {
    match notice {
        Housekeeping::Suppressed(count) => monitor::suppressed(*count),
        Housekeeping::TooMuchOutput {
            suppressed,
            seconds,
        } => monitor::too_much_output(*suppressed, *seconds),
        Housekeeping::Expired { timeout_ms, events } => monitor::expired(
            &rust_cli_kernel_api::metering::format_duration_trimmed(*timeout_ms),
            *events,
        ),
        Housekeeping::Socket(SocketEvent::Error { message }) => monitor::ws_error(message),
        Housekeeping::Socket(SocketEvent::Rejected { status }) => monitor::ws_rejected(*status),
        Housekeeping::Socket(SocketEvent::Dropped { bytes }) => monitor::ws_dropped(*bytes),
        Housekeeping::Socket(SocketEvent::Closed { code, reason }) => {
            monitor::ws_closed(*code, reason)
        }
        Housekeeping::Stopped => monitor::stopped_notice().to_owned(),
    }
}

#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;
