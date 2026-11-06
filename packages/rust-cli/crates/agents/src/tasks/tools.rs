//! TaskCreate / TaskGet / TaskList / TaskUpdate（结果文本逐字照写）。
//! 描述与 schema 在 `prompts/tools/Task*.json`。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::agents::TaskList;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, parse,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::tasks::edit::{CreateInput, GetInput, UpdateInput, create, get, list_text, update};
use crate::{Core, TaskTools};
use rust_cli_kernel_api::tool_names;

pub(crate) fn tools(core: &Arc<Core>, switch: &TaskTools) -> Vec<Arc<dyn Tool>> {
    [Kind::Create, Kind::Get, Kind::List, Kind::Update]
        .into_iter()
        .map(|kind| {
            Arc::new(TaskTool {
                core: core.clone(),
                kind,
                switch: switch.clone(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Create,
    Get,
    List,
    Update,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Create => tool_names::TASK_CREATE,
            Self::Get => tool_names::TASK_GET,
            Self::List => tool_names::TASK_LIST,
            Self::Update => tool_names::TASK_UPDATE,
        }
    }
}

struct TaskTool {
    core: Arc<Core>,
    kind: Kind,
    switch: TaskTools,
}

fn failed(error: impl std::fmt::Display) -> ToolError {
    ToolError::Failed {
        message: error.to_string(),
    }
}

impl TaskTool {
    fn list_id(&self, context: &ToolUseContext) -> SessionId {
        self.core.live.root_of(&context.session)
    }

    /// 写成功后把新清单交给顶层会话（`TasksChanged`）。
    fn announce(&self, list: &SessionId, tasks: Option<TaskList>) {
        if let Some(tasks) = tasks {
            let value = serde_json::to_value(&tasks.tasks).unwrap_or(Value::Null);
            self.core.hub.tasks_changed(list, value);
        }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        self.kind.name()
    }

    fn is_enabled(&self) -> bool {
        self.switch.enabled()
    }

    /// 延迟加载（docs/specs/rust-cli/tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        true
    }

    /// ToolSearch 的关键词提示。
    fn search_hint(&self) -> Option<&str> {
        Some(match self.kind {
            Kind::Create => "create a task in the task list",
            Kind::Get => "retrieve a task by ID",
            Kind::List => "list all tasks",
            Kind::Update => "update a task",
        })
    }

    fn input_schema(&self) -> Value {
        rust_cli_prompts::definition(self.kind.name())
            .schema
            .clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        rust_cli_prompts::definition(self.kind.name())
            .description
            .clone()
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        matches!(self.kind, Kind::Get | Kind::List)
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        matches!(self.kind, Kind::Get | Kind::List)
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(rust_cli_kernel_api::tool::DEFAULT_MAX_RESULT_CHARS)
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        // 工具的缺省：任务清单不经权限询问。
        PermissionDecision::Allow {
            updated_input: None,
        }
    }

    async fn call(
        &self,
        value: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let list = self.list_id(&context);
        let board = &self.core.board;
        let text = match self.kind {
            Kind::Create => {
                let input: CreateInput = parse(&value)?;
                let (text, tasks) = board
                    .update(&list, Box::new(move |tasks| (create(tasks, input), true)))
                    .await
                    .map_err(failed)?;
                self.announce(&list, tasks);
                text
            }
            Kind::Update => {
                let input: UpdateInput = parse(&value)?;
                let (text, tasks) = board
                    .update(&list, Box::new(move |tasks| update(tasks, input)))
                    .await
                    .map_err(failed)?;
                self.announce(&list, tasks);
                text
            }
            Kind::Get => {
                let input: GetInput = parse(&value)?;
                get(&board.read(&list).await.map_err(failed)?, &input.task_id)
            }
            Kind::List => list_text(&board.read(&list).await.map_err(failed)?),
        };
        Ok(rust_cli_kernel_api::tool::text(text))
    }
}
