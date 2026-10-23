//! ScheduleWakeup（docs/specs/rust-cli/tools-more.md §17.2）。
//!
//! `/loop` 动态模式的自定节奏：模型每轮结束前调用它，排一个 60–3600 秒后的 loop 唤醒（任务表的会话任务，
//! [`CronBoard::schedule_wakeup`]），到点后同一个 prompt 再进会话；`stop: true` 结束循环。描述与 schema 逐字
//! 取自 `prompts/tools/ScheduleWakeup.json`。

use async_trait::async_trait;
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{Definition, definition};
use crate::cron_board::CronBoard;
use crate::cron_loop::Scheduled;
use crate::cron_texts;
use crate::cron_tools::{job_id, output, owner};

pub const NAME: &str = "ScheduleWakeup";
/// 搜索提示（searchHint）。
const SEARCH_HINT: &str = "self-pace the dynamic /loop: pick a delay before the next tick, or stop/end/cancel the dynamic loop with stop:true (a fixed-interval /loop is a recurring cron \u{2014} cancel it with CronDelete)";

pub struct ScheduleWakeupTool {
    definition: Definition,
    board: CronBoard,
}

impl ScheduleWakeupTool {
    pub fn new(board: &CronBoard) -> Self {
        Self {
            definition: definition(NAME),
            board: board.clone(),
        }
    }
}

/// 数字或数字字符串。
fn seconds(input: &Value) -> Option<f64> {
    match input.get("delaySeconds")? {
        Value::Number(number) => number.as_f64(),
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    }
}

fn failed(message: &str) -> ToolError {
    ToolError::Failed {
        message: message.to_owned(),
    }
}

#[async_trait]
impl Tool for ScheduleWakeupTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn search_hint(&self) -> Option<&str> {
        Some(SEARCH_HINT)
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(1_000)
    }

    /// auto 模式交给分类器（passthrough），其余直接放行。
    async fn check_permissions(
        &self,
        _input: &Value,
        context: &ToolUseContext,
    ) -> PermissionDecision {
        if context.mode == PermissionMode::Auto {
            return PermissionDecision::Passthrough;
        }
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let owner = owner(&context).await;
        let text = |key: &str| input.get(key).and_then(Value::as_str).map(str::to_owned);
        if input.get("stop").and_then(Value::as_bool) == Some(true) {
            let cancelled = self.board.stop_loop(&owner);
            let display = json!({"scheduledFor": 0, "clampedDelaySeconds": 0, "wasClamped": false,
                "stopped": true, "cancelledWakeups": cancelled});
            return Ok(output(cron_texts::loop_stopped(cancelled), display));
        }
        let (Some(delay), Some(_reason)) = (seconds(&input), text("reason")) else {
            return Err(failed(
                "`delaySeconds` and `reason` are required when `stop` is not true.",
            ));
        };
        let Some(prompt) = text("prompt") else {
            return Err(failed("`prompt` is required when `stop` is not true."));
        };
        if input.get("noop").and_then(Value::as_bool).is_none() {
            return Err(failed("`noop` is required when `stop` is not true."));
        }
        let scheduled = self
            .board
            .schedule_wakeup(&owner, job_id(&context), delay, &prompt);
        let Scheduled::At {
            scheduled_for,
            clamped,
            was_clamped,
        } = scheduled
        else {
            let display = json!({"scheduledFor": 0, "clampedDelaySeconds": 0, "wasClamped": false});
            return Ok(output(cron_texts::wakeup_ended(), display));
        };
        let now = self.board.now_ms();
        // JS `Math.round`：.5 向正无穷。
        let in_seconds = (scheduled_for.saturating_sub(now) + 500) / 1_000;
        let at = cron_texts::clock_time(scheduled_for, &self.board.offset());
        let text = cron_texts::wakeup_scheduled(&at, in_seconds, was_clamped.then_some(clamped));
        let display = json!({"scheduledFor": scheduled_for, "clampedDelaySeconds": clamped,
            "wasClamped": was_clamped});
        Ok(output(text, display))
    }
}

#[cfg(test)]
#[path = "schedule_wakeup_tests.rs"]
mod tests;
