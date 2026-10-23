//! CronList（docs/specs/rust-cli/tools-more.md §10.2、§16.6）。
//!
//! 先列文件里的全部持久任务（不分创建者），再列本会话的会话任务（含 ScheduleWakeup 的
//! 唤醒）；只有会话任务带 ` [session-only]`。

use async_trait::async_trait;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use unicode_width::UnicodeWidthStr;

use crate::cron_expr;
use crate::cron_tools::{LIST, Shared, output, owner, shared_tool_methods};

pub struct CronListTool(pub(crate) Shared);

/// 列表里的一行：持久任务的 `durable` 不写（`durable === false` 才标 session-only）。
struct Row {
    id: String,
    cron: String,
    prompt: String,
    recurring: bool,
    session_only: bool,
}

#[async_trait]
impl Tool for CronListTool {
    fn name(&self) -> &str {
        LIST
    }

    shared_tool_methods!();

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let board = &self.0.board;
        let durable = board.read_durable().await.into_iter().map(|task| Row {
            id: task.id,
            cron: task.cron,
            prompt: task.prompt,
            recurring: task.recurring,
            session_only: false,
        });
        let session = board
            .session_jobs(&owner(&context).await)
            .into_iter()
            .map(|job| Row {
                id: job.id,
                cron: job.cron,
                prompt: job.prompt,
                recurring: job.recurring,
                session_only: true,
            });
        let rows: Vec<Row> = durable.chain(session).collect();
        let lines: Vec<String> = rows
            .iter()
            .map(|row| {
                let kind = if row.recurring {
                    " (recurring)"
                } else {
                    " (one-shot)"
                };
                let only = if row.session_only {
                    " [session-only]"
                } else {
                    ""
                };
                let human = cron_expr::human(&row.cron);
                let prompt = truncate_display(&row.prompt, 80);
                format!("{} \u{2014} {human}{kind}{only}: {prompt}", row.id)
            })
            .collect();
        let text = if lines.is_empty() {
            "No scheduled jobs.".to_owned()
        } else {
            lines.join("\n")
        };
        let display = json!({"jobs": rows.iter().map(|row| json!({
            "id": row.id, "cron": row.cron, "humanSchedule": cron_expr::human(&row.cron),
            "prompt": row.prompt, "recurring": row.recurring, "durable": !row.session_only,
        })).collect::<Vec<_>>()});
        Ok(output(text, display))
    }
}

/// 按显示宽度截断（只取首行，有后续行时加 `…`；按字符近似字素）。
pub(crate) fn truncate_display(text: &str, columns: usize) -> String {
    if let Some((first, _)) = text.split_once('\n') {
        if first.width() + 1 > columns {
            return truncate_width(&format!("{first}\u{2026}"), columns);
        }
        return format!("{first}\u{2026}");
    }
    if text.width() <= columns {
        return text.to_owned();
    }
    truncate_width(text, columns)
}

/// 按显示宽度截断：留一列给 `…`。
fn truncate_width(text: &str, columns: usize) -> String {
    if text.width() <= columns {
        return text.to_owned();
    }
    if columns <= 1 {
        return "\u{2026}".to_owned();
    }
    let mut used = 0;
    let mut kept = String::new();
    for character in text.chars() {
        let width = unicode_width::UnicodeWidthChar::width(character).unwrap_or(0);
        if used + width > columns - 1 {
            break;
        }
        kept.push(character);
        used += width;
    }
    kept + "\u{2026}"
}
