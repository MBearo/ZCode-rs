//! EnterPlanMode / ExitPlanMode（docs/specs/rust-cli/tools-more.md §2）。
//!
//! 两个工具都不持有执行状态：经 `Interactions::set_plan` 请求会话 actor 打开或关掉 plan 开关（唯一 owner），
//! 返回后同一回合里后续的调用按新状态判决（§2.3）。修复线 F20：plan 是独立开关，权限模式留在 actor 里，退出
//! plan 后就是进入前的值（Node `exitPlanMode` 只设 `planEnabled: false`）。计划文件经执行环境端口读（§2.2）。

use async_trait::async_trait;
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::{ReadText, TextRead};
use rust_cli_kernel_api::orchestration::{InteractionRequest, PlanApprovalAnswer};
use rust_cli_kernel_api::permission::{PermissionDecision, PermissionMode};
use rust_cli_kernel_api::plan::PlanFiles;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::ask_user_question::user_interaction;
use crate::common::{Definition, definition, failed, text};
use crate::pipeline::{REJECT_MESSAGE, REJECT_WITH_REASON, rejected};
use crate::texts_more as texts;

pub const ENTER: &str = "EnterPlanMode";
pub const EXIT: &str = "ExitPlanMode";

/// 描述里探索工具的写法：Glob / Grep
/// 不在工具清单里（由 `find` / `grep` 代替）时写 `find`/Glob。
///
/// 修 bug（修复线 F9）：原来按“这次请求的工具里有 Bash”判断，`/context` 的计数请求只带
/// EnterPlanMode 一个工具时写错；改为与 Bash 提示同一条规则（tools-more.md §1）。
fn explore_tools(context: &PromptContext) -> &'static str {
    let search_tools = context
        .tool_names
        .iter()
        .any(|name| name == crate::glob::NAME || name == crate::grep::NAME);
    if search_tools {
        "Glob, Grep, and Read"
    } else {
        "`find`/Glob, `grep`/Grep, and Read"
    }
}

fn invalid(message: &str) -> Validation {
    Validation::Invalid {
        message: message.to_owned(),
        error_code: Some(1),
    }
}

async fn set_plan(context: &ToolUseContext, enabled: bool) -> Result<(), ToolError> {
    context
        .interactions
        .set_plan(&context.session, enabled)
        .await
        .map_err(|error| failed(error.to_string()))
}

pub struct EnterPlanModeTool {
    definition: Definition,
    interactive: bool,
}

impl EnterPlanModeTool {
    /// `interactive`：EnterPlanMode 跟随 ExitPlanMode 的可用性，非交互会话不提供。
    pub fn new(interactive: bool) -> Self {
        Self {
            definition: definition(ENTER),
            interactive,
        }
    }
}

#[async_trait]
impl Tool for EnterPlanModeTool {
    fn name(&self) -> &str {
        ENTER
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, context: &PromptContext) -> String {
        self.definition
            .description
            .replace("{{explore_tools}}", explore_tools(context))
    }

    fn is_enabled(&self) -> bool {
        self.interactive
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("switch to plan mode to design an approach before coding")
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        true
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(100_000)
    }

    async fn validate_input(&self, _input: &Value, context: &ToolUseContext) -> Validation {
        if context.agent.is_some() {
            return invalid(texts::PLAN_MODE_MAIN_ONLY);
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
        _input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        set_plan(&context, true).await?;
        Ok(text(texts::ENTERED_PLAN_MODE.to_owned()))
    }
}

pub struct ExitPlanModeTool {
    definition: Definition,
    interactive: bool,
}

impl ExitPlanModeTool {
    pub fn new(interactive: bool) -> Self {
        Self {
            definition: definition(EXIT),
            interactive,
        }
    }
}

/// 计划文件的位置（§2.2）：kernel-api 的 `PlanFiles`，与计划模式 attachment、判决链同一来源。
/// Node 的位置 `<工作区>/.zcode/plans/plan-<会话>.md`；会话 ID 净化后为空时报错（Node 同样报错）。
pub(crate) async fn plan_file(context: &ToolUseContext) -> Result<String, ToolError> {
    let facts = context
        .machine
        .info()
        .path_facts(&context.workspace)
        .await
        .map_err(|error| failed(error.to_string()))?;
    let plans = PlanFiles::in_workspace(&facts, &context.workspace.root, &context.session)
        .ok_or_else(|| {
            failed("cannot locate the plan file: the session id cannot name a file".to_owned())
        })?;
    Ok(plans.path(context.agent.as_ref()).as_str().to_owned())
}

/// 读计划文件；不存在或读不了时没有计划。
async fn read_plan(context: &ToolUseContext, path: &str) -> Option<String> {
    let request = ReadText {
        path: EnvPath::new(path),
        start_line: 0,
        max_lines: None,
        max_bytes: None,
    };
    match context
        .machine
        .files()
        .read_text(&context.workspace, request)
        .await
    {
        Ok(TextRead::Text(slice)) => Some(slice.content),
        Ok(TextRead::Directory | TextRead::TooLarge { .. }) | Err(_) => None,
    }
}

#[async_trait]
impl Tool for ExitPlanModeTool {
    fn name(&self) -> &str {
        EXIT
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn is_enabled(&self) -> bool {
        self.interactive
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("present plan for approval and start coding (plan mode only)")
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(100_000)
    }

    async fn validate_input(&self, _input: &Value, context: &ToolUseContext) -> Validation {
        if context.mode != PermissionMode::Plan {
            return invalid(texts::NOT_IN_PLAN_MODE);
        }
        Validation::Ok
    }

    async fn check_permissions(
        &self,
        _input: &Value,
        context: &ToolUseContext,
    ) -> PermissionDecision {
        user_interaction(context.mode, texts::EXIT_PLAN_MODE)
    }

    async fn call(
        &self,
        _input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let path = plan_file(&context).await?;
        let plan = read_plan(&context, &path).await.unwrap_or_default();
        let request = InteractionRequest::PlanApproval {
            tool_use: context.tool_use.clone(),
            plan: plan.clone(),
        };
        let Ok(answer) = crate::asking::ask::<PlanApprovalAnswer>(&context, request).await else {
            return Err(ToolError::Cancelled);
        };
        match answer {
            // 修复线 F20：批准只关掉 plan，回答里的 `mode` 只作记录。
            Some(PlanApprovalAnswer::Approve { .. }) => {}
            Some(PlanApprovalAnswer::KeepPlanning {
                feedback: Some(feedback),
            }) if !feedback.trim().is_empty() => {
                return Ok(rejected(format!("{REJECT_WITH_REASON}{feedback}")));
            }
            Some(PlanApprovalAnswer::KeepPlanning { .. }) | None => {
                return Ok(rejected(REJECT_MESSAGE.to_owned()));
            }
        }
        if context.agent.is_some() {
            // 子代理的模式不归会话 actor 管：只回报批准。
            return Ok(text(texts::AGENT_PLAN_APPROVED.to_owned()));
        }
        set_plan(&context, false).await?;
        if plan.trim().is_empty() {
            return Ok(text(texts::PLAN_EXIT_APPROVED.to_owned()));
        }
        Ok(text(texts::plan_approved(&path, &plan)))
    }
}

#[cfg(test)]
#[path = "plan_mode_tests.rs"]
mod tests;
