//! `task_reminder` attachment（docs/specs/rust-cli/agents.md §7.4）：任务工具很久没用时的提醒，附上现在的任务。
//!
//! 轮数按 assistant 的块数（不算 thinking）：从最近一次 TaskCreate / TaskUpdate 调用之后、最近一次提醒之后各数一遍，
//! 两个都到了 [`TURNS`] 才提醒。任务清单由 agents 给出（kernel-api `TaskReader`）。

use rust_cli_kernel_api::agents::TaskRecord;
use rust_cli_kernel_api::message::{ContentBlock, Message};
use rust_cli_prompts::texts;
use serde_json::{Value, json};

/// 来源（attachment type）。
pub(crate) const SOURCE: &str = "task_reminder";
/// 提醒要的轮数（上次用任务工具之后、上次提醒之后都要到）。
const TURNS: usize = 10;
/// 算作“用过任务工具”的调用。
const TASK_TOOLS: [&str; 2] = ["TaskCreate", "TaskUpdate"];
/// 有这个工具时才提醒。
pub(crate) const REQUIRED_TOOL: &str = "TaskUpdate";

/// （上次用任务工具之后的轮数，上次提醒之后的轮数）。
fn turns(history: &[Message]) -> (usize, usize) {
    let (mut since_use, mut since_reminder) = (0, 0);
    let (mut used, mut reminded) = (false, false);
    for message in history.iter().rev() {
        match message {
            Message::Assistant(assistant) => {
                for block in assistant.content.iter().rev() {
                    if matches!(
                        block,
                        ContentBlock::Thinking { .. } | ContentBlock::RedactedThinking { .. }
                    ) {
                        continue;
                    }
                    if !used
                        && matches!(block, ContentBlock::ToolUse { name, .. } if TASK_TOOLS.contains(&name.as_str()))
                    {
                        used = true;
                    }
                    if !used {
                        since_use += 1;
                    }
                    if !reminded {
                        since_reminder += 1;
                    }
                }
            }
            Message::Attachment(attachment) if !reminded && attachment.source == SOURCE => {
                reminded = true;
            }
            _ => {}
        }
        if used && reminded {
            break;
        }
    }
    (since_use, since_reminder)
}

/// 这一步该不该提醒：`history` 是最近一次压缩边界之后的部分。
pub(crate) fn due(history: &[Message]) -> bool {
    if history.is_empty() {
        return false;
    }
    let (since_use, since_reminder) = turns(history);
    since_use >= TURNS && since_reminder >= TURNS
}

/// 提醒的文字与数据。
pub(crate) fn render(tasks: &[TaskRecord]) -> (String, Value) {
    let mut text = format!("{}\n", texts::task_reminder());
    if !tasks.is_empty() {
        let lines = tasks
            .iter()
            .map(|task| format!("#{}. [{}] {}", task.id, task.status.as_str(), task.subject))
            .collect::<Vec<_>>()
            .join("\n");
        text.push_str("\n\n");
        text.push_str(&texts::render(
            texts::task_reminder_tasks(),
            &[("tasks", &lines)],
        ));
    }
    (text, json!({"content": tasks, "itemCount": tasks.len()}))
}

#[cfg(test)]
#[path = "task_reminder_tests.rs"]
mod tests;
