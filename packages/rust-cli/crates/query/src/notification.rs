//! 后台命令终态通知的文本（复制自 tools 的 `bash_output`，L4a 实现；移到这里是因为通知由会话
//! actor 交给主循环，tools 不再需要它）。

use rust_cli_kernel_api::env::Outcome;
use rust_cli_kernel_api::env_shell::ShellNotice;

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

#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;
