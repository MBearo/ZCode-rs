//! CronCreate / CronDelete / CronList（docs/specs/rust-cli/tools-more.md §10.2）。
//!
//! 工具只改任务表（[`CronBoard`]），任务记在顶层会话名下（`Interactions::owner_session`，前台子代理
//! 转给父会话）。触发由会话 actor 在空闲时取走到期任务（§10.4）。`durable: true` 的任务写进
//! `.zcode/scheduled_tasks.json`（§16）；计数、删除校验与列出每次重读文件。

use async_trait::async_trait;
use rust_cli_base::names::{DOT_DIR, SCHEDULED_TASKS_FILE};
use rust_cli_kernel_api::cron_store::NewCronTask;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::common::{Definition, definition, parse};
use crate::cron_board::{CronBoard, MAX_JOBS};
use crate::cron_expr;

pub const CREATE: &str = "CronCreate";
pub const DELETE: &str = "CronDelete";
pub const LIST: &str = "CronList";

/// CronCreate 结果的两种说明之一（另一种 `Persisted to
/// .zcode/scheduled_tasks.json` 在结果里用共用的名字拼）。产品名按替换规则换成 ZCode（2026-10-18 用户确认，
/// tools-more.md §10.2），路径换成 `.zcode`，其余逐字。
const SESSION_ONLY: &str = "Session-only (not written to disk, dies when ZCode exits)";
/// 循环任务的有效天数（有效期 / 1 天）。
const MAX_AGE_DAYS: u64 = crate::cron_expr::RECURRING_MAX_AGE_MS / 86_400_000;

pub(crate) async fn owner(context: &ToolUseContext) -> SessionId {
    context.interactions.owner_session(&context.session).await
}

/// 任务 ID：取这次调用的操作 ID（`op_<uuid>`）里 uuid 的前 8 位。
pub(crate) fn job_id(context: &ToolUseContext) -> String {
    let operation = context.operation.operation.as_str();
    let uuid = operation.rsplit('_').next().unwrap_or(operation);
    uuid.chars().take(8).collect()
}

pub(crate) fn output(content: String, display: Value) -> ToolOutput {
    let mut output = crate::common::text(content);
    output.display = display;
    output
}

/// 三个工具共有的部分：定义、任务表、延迟加载与提示词。
pub(crate) struct Shared {
    pub(crate) definition: Definition,
    pub(crate) board: CronBoard,
    pub(crate) hint: &'static str,
}

impl Shared {
    fn new(name: &str, board: &CronBoard, hint: &'static str) -> Self {
        Self {
            definition: definition(name),
            board: board.clone(),
            hint,
        }
    }
}

/// 三个 cron 工具（按工具池的顺序）。
pub fn cron_tools(board: &CronBoard) -> Vec<std::sync::Arc<dyn Tool>> {
    vec![
        std::sync::Arc::new(CronCreateTool(Shared::new(
            CREATE,
            board,
            "schedule a recurring or one-shot prompt",
        ))),
        std::sync::Arc::new(CronDeleteTool(Shared::new(
            DELETE,
            board,
            "cancel a scheduled cron job",
        ))),
        std::sync::Arc::new(crate::cron_list::CronListTool(Shared::new(
            LIST,
            board,
            "list active cron jobs",
        ))),
    ]
}

macro_rules! shared_tool_methods {
    () => {
        fn input_schema(&self) -> Value {
            self.0.definition.schema.clone()
        }

        fn description(&self, _context: &PromptContext) -> String {
            self.0.definition.description.clone()
        }

        fn should_defer(&self) -> bool {
            true
        }

        fn search_hint(&self) -> Option<&str> {
            Some(self.0.hint)
        }

        fn max_result_size_chars(&self) -> Option<usize> {
            Some(100_000)
        }
    };
}
pub(crate) use shared_tool_methods;

#[derive(Deserialize)]
struct CreateInput {
    cron: String,
    prompt: String,
    #[serde(default)]
    recurring: Option<bool>,
    #[serde(default)]
    durable: Option<bool>,
}

pub struct CronCreateTool(Shared);

#[async_trait]
impl Tool for CronCreateTool {
    fn name(&self) -> &str {
        CREATE
    }

    shared_tool_methods!();

    async fn validate_input(&self, input: &Value, context: &ToolUseContext) -> Validation {
        let Ok(input) = parse::<CreateInput>(input) else {
            return Validation::Ok;
        };
        if cron_expr::parse(&input.cron).is_none() {
            return coded(
                format!(
                    "Invalid cron expression '{}'. Expected 5 fields: M H DoM Mon DoW.",
                    input.cron
                ),
                1,
            );
        }
        let board = &self.0.board;
        if cron_expr::next_after(&input.cron, board.now_ms(), &board.offset()).is_none() {
            return coded(
                format!(
                    "Cron expression '{}' does not match any calendar date in the next year.",
                    input.cron
                ),
                2,
            );
        }
        let total = board.read_durable().await.len() + board.session_count(&owner(context).await);
        if total >= MAX_JOBS {
            return coded(
                format!("Too many scheduled jobs (max {MAX_JOBS}). Cancel one first."),
                3,
            );
        }
        Validation::Ok
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
        let input: CreateInput = parse(&input)?;
        let recurring = input.recurring.unwrap_or(true);
        let id = job_id(&context);
        let owner = owner(&context).await;
        let board = &self.0.board;
        let durable = input.durable.unwrap_or(false)
            && board
                .add_durable(NewCronTask {
                    id: id.clone(),
                    cron: input.cron.clone(),
                    prompt: input.prompt.clone(),
                    created_at: board.now_ms(),
                    recurring,
                    created_by: owner.clone(),
                })
                .await
                .map_err(|message| ToolError::Failed { message })?;
        if !durable {
            board.add(&owner, id.clone(), &input.cron, &input.prompt, recurring);
        }
        let human = cron_expr::human(&input.cron);
        // 落盘的位置取共用的名字（F55，与 env-local 的定时任务存储同一个文件）；文字不变。
        let note = if durable {
            format!("Persisted to {DOT_DIR}/{SCHEDULED_TASKS_FILE}")
        } else {
            SESSION_ONLY.to_owned()
        };
        let text = if recurring {
            format!(
                "Scheduled recurring job {id} ({human}). {note}. Auto-expires after {MAX_AGE_DAYS} days. Use CronDelete to cancel sooner."
            )
        } else {
            format!(
                "Scheduled one-shot task {id} ({human}). {note}. It will fire once then auto-delete."
            )
        };
        let display =
            json!({"id": id, "humanSchedule": human, "recurring": recurring, "durable": durable});
        Ok(output(text, display))
    }
}

pub(crate) fn coded(message: String, code: i32) -> Validation {
    Validation::Invalid {
        message,
        error_code: Some(code),
    }
}

#[derive(Deserialize)]
struct DeleteInput {
    id: String,
}

pub struct CronDeleteTool(Shared);

#[async_trait]
impl Tool for CronDeleteTool {
    fn name(&self) -> &str {
        DELETE
    }

    shared_tool_methods!();

    async fn validate_input(&self, input: &Value, context: &ToolUseContext) -> Validation {
        let Ok(input) = parse::<DeleteInput>(input) else {
            return Validation::Ok;
        };
        let board = &self.0.board;
        let known = board.has_session_job(&owner(context).await, &input.id)
            || board
                .read_durable()
                .await
                .iter()
                .any(|task| task.id == input.id);
        if !known {
            return coded(format!("No scheduled job with id '{}'", input.id), 1);
        }
        Validation::Ok
    }

    /// 没有专属的权限检查，默认放行。
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
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let input: DeleteInput = parse(&input)?;
        self.0
            .board
            .delete(&owner(&context).await, &input.id)
            .await
            .map_err(|message| ToolError::Failed { message })?;
        Ok(output(
            format!("Cancelled job {}.", input.id),
            json!({"id": input.id}),
        ))
    }
}

#[cfg(test)]
#[path = "cron_tools_tests.rs"]
mod tests;
