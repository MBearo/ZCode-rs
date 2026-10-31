//! TaskStop（docs/specs/rust-cli/agents.md §6）：停
//! 本会话的后台子代理，或经执行环境停后台命令。描述与 schema 在 `prompts/tools/TaskStop.json`。
//!
//! 后台命令的登记只在执行环境的账本里（shell-permissions.md §2.6），这里每次都去问，不另记任务表。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::OperationContext;
use rust_cli_kernel_api::env_shell::{BackgroundStatus, BackgroundStop, BackgroundTask};
use rust_cli_kernel_api::ids::{AgentId, OperationId, SessionId, ShellTaskId};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::Core;
use crate::agent_tool::text_output;
use crate::live::{AgentState, LiveAgent};
use crate::routing;

const NAME: &str = "TaskStop";

#[derive(Deserialize)]
struct Input {
    #[serde(default)]
    task_id: Option<String>,
    #[serde(default)]
    shell_id: Option<String>,
}

/// 要停的对象。
enum Target {
    /// 子代理的 ID 与 description。
    Agent(AgentId, String),
    /// 执行环境里还在跑的后台命令，带它所在的会话（主线程可能停子代理的命令）与机器（R3）。
    Command(Found),
}

/// 结果（`JSON.stringify` 输出对象，键序固定）。
#[derive(Serialize)]
struct Stopped<'a> {
    message: String,
    task_id: &'a str,
    task_type: &'a str,
    command: &'a str,
    /// 远端机器上的命令所在的机器（F98，remote-exec.md §25.2.4）；本机不写。
    #[serde(skip_serializing_if = "Option::is_none")]
    machine: Option<&'a str>,
}

/// 显示用的命令最多这么多个字符。
const MAX_SHOWN_CHARS: usize = 160;

/// 显示用的命令：去掉控制字符、空白合成一个空格、首尾去空、超过 160 个字符时截断加 `…`。
fn shown(text: &str) -> String {
    let clean: String = text
        .chars()
        .filter(|c| c.is_whitespace() || !c.is_control())
        .collect();
    let joined = clean.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() > MAX_SHOWN_CHARS {
        let head: String = joined.chars().take(MAX_SHOWN_CHARS).collect();
        format!("{head}\u{2026}")
    } else {
        joined
    }
}

fn task_id(input: &Value) -> Option<String> {
    let input: Input = serde_json::from_value(input.clone()).ok()?;
    input.task_id.or(input.shell_id).filter(|id| !id.is_empty())
}

pub(crate) struct TaskStopTool {
    pub(crate) core: Arc<Core>,
}

/// 发起停止的一方：TaskStop 工具（模型，可能在子代理里）或用户（`/tasks`，主线程）。两者走同一条路径
/// （docs/specs/rust-cli/tui-display.md §3.1）。
pub(crate) struct Caller<'a> {
    pub(crate) session: &'a SessionId,
    pub(crate) agent: Option<&'a AgentId>,
    pub(crate) machine: &'a MachineEnv,
    pub(crate) operation: &'a OperationContext,
}

/// 后台命令所在的会话与机器（R3：TaskStop 按任务所在的机器停，remote-exec.md §20.7.2）。
struct Found {
    session: SessionId,
    machine: MachineEnv,
}

impl<'a> Caller<'a> {
    fn of(context: &'a ToolUseContext) -> Self {
        Self {
            session: &context.session,
            agent: context.agent.as_ref(),
            machine: &context.machine,
            operation: &context.operation,
        }
    }
}

/// 成功停止：（任务 ID、`local_agent` / `local_bash`、描述或命令、远端的机器）。
pub(crate) struct StoppedTask {
    pub(crate) id: String,
    pub(crate) task_type: &'static str,
    pub(crate) command: String,
    /// 命令在远端机器上时是那台机器的名字（F98）；本机与子代理为 None。
    pub(crate) machine: Option<String>,
}

impl StoppedTask {
    /// 结果里的 `message`。
    pub(crate) fn message(&self) -> String {
        format!("Successfully stopped task: {} ({})", self.id, self.command)
    }
}

impl TaskStopTool {
    /// 能看到的子代理：主线程看同一个顶层会话里的全部，子代理只看自己启动的。
    fn visible(&self, agent: &LiveAgent, context: &Caller<'_>) -> bool {
        match context.agent {
            None => agent.root == *context.session,
            Some(_) => agent.parent == *context.session,
        }
    }

    /// `No task found with ID: <id>`，有在跑的后台子代理时列出它们。
    fn not_found(&self, id: &str, context: &Caller<'_>) -> String {
        let root = self.core.live.root_of(context.session);
        let running: Vec<String> = self
            .core
            .live
            .background_in(&root)
            .into_iter()
            .filter(|agent| Some(&agent.agent) != context.agent)
            .map(|agent| {
                format!(
                    "{} ({})",
                    agent.agent,
                    shown(&agent.description).replace('`', "'")
                )
            })
            .collect();
        let mut text = format!("No task found with ID: {}", shown(id));
        if !running.is_empty() {
            text.push_str(&format!(
                ". Running background agents: {}",
                running.join(", ")
            ));
        }
        text
    }

    /// 先找子代理，再问执行环境的后台命令；找不到或不在跑时是校验错误的文本。
    async fn find(&self, id: &str, context: &Caller<'_>) -> Result<Target, String> {
        let not_running = |status: &str| format!("Task {id} is not running (status: {status})");
        let agent = self
            .core
            .live
            .get(&AgentId::new(id))
            .filter(|agent| self.visible(agent, context));
        if let Some(agent) = agent {
            return match agent.state {
                AgentState::Finished(ending) => Err(not_running(ending.status())),
                // 停放的也能停（agents.md §5.1）。
                AgentState::Running { .. } | AgentState::Parked(_) => {
                    Ok(Target::Agent(agent.agent, agent.description))
                }
            };
        }
        // 依次问本机与连上的远端机器（暂定，remote-exec.md §20.7.2）：任务 ID 由 operationId 派生，跨机器不重复。
        for machine in self.machines(context) {
            for session in self.command_sessions(context) {
                let key = BackgroundTask {
                    session: session.clone(),
                    task: ShellTaskId::new(id),
                };
                match machine.background().background_command(key).await {
                    Ok(Some(info)) if info.status == BackgroundStatus::Running => {
                        return Ok(Target::Command(Found { session, machine }));
                    }
                    Ok(Some(info)) => return Err(not_running(info.status.as_str())),
                    // 执行环境不支持后台命令时同“不认识”。
                    Ok(None) | Err(_) => {}
                }
            }
        }
        Err(self.not_found(id, context))
    }

    /// 要问的机器：调用方的（本机），再是连上的远端机器（按名字）。
    fn machines(&self, context: &Caller<'_>) -> Vec<MachineEnv> {
        let registry = &self.core.ports.machines;
        let mut machines = vec![context.machine.clone()];
        machines.extend(
            registry
                .machines()
                .machines
                .iter()
                .filter_map(|remote| registry.machine(&remote.name)),
        );
        machines
    }

    /// 停止停放的子代理时，停掉它子会话里还在跑的后台命令（L18：
    /// `killing orphaned shell task`）。它派生的后台子代理不停（不级联停止子代理）。每次停止的 operationId
    /// 由这次 TaskStop 的 operationId 加任务 ID 派生，重放时相同（README §6 规则 3）；已结束、不认识的忽略。
    async fn stop_commands_of(&self, agent: &AgentId, context: &Caller<'_>) {
        let Some(entry) = self.core.live.get(agent) else {
            return;
        };
        let Ok(state) = context
            .machine
            .shell()
            .shell_state(&entry.workspace, &entry.child)
            .await
        else {
            return;
        };
        for task in state.background {
            let mut operation = context.operation.clone();
            operation.operation = OperationId::new(format!("{}_{}", operation.operation, task));
            let key = BackgroundTask {
                session: entry.child.clone(),
                task,
            };
            if let Err(error) = context
                .machine
                .background()
                .stop_background_command(operation, key)
                .await
            {
                tracing::warn!(%error, "a parked agent's command was not stopped");
            }
        }
    }

    /// 按顺序去问哪些会话的后台命令：自己的；主线程再加这个顶层会话里各子代理的子会话。
    ///
    /// 修正（L18）：以前只问本会话，主线程停子代理启动的命令时回 `No task found`。任务表按进程看，
    /// 主线程能停任何任务，子代理只停自己的。账本仍只在执行环境里，
    /// 这里逐个子会话问，不另记任务表。
    fn command_sessions(&self, context: &Caller<'_>) -> Vec<SessionId> {
        let mut sessions = vec![context.session.clone()];
        if context.agent.is_none() {
            sessions.extend(self.core.live.with(|agents| {
                agents
                    .values()
                    .filter(|agent| agent.root == *context.session)
                    .map(|agent| agent.child.clone())
                    .collect::<Vec<_>>()
            }));
        }
        sessions
    }

    /// 停止一个任务（TaskStop 的 `call` 与用户的 `/tasks` 共用）。校验之后状态可能变了（命令刚好自己结束）：
    /// 按执行时的结果报错（StopTaskError）。
    pub(crate) async fn stop(
        &self,
        id: &str,
        context: &Caller<'_>,
    ) -> Result<StoppedTask, ToolError> {
        match self.find(id, context).await {
            Ok(Target::Agent(agent, description)) => {
                if !self.core.live.stop(&agent)
                    && let Some(wakes) = routing::stop_parked(&self.core, &agent)
                {
                    self.stop_commands_of(&agent, context).await;
                    routing::wake(&self.core, wakes).await;
                }
                Ok(StoppedTask {
                    id: id.to_owned(),
                    task_type: "local_agent",
                    command: description,
                    machine: None,
                })
            }
            Ok(Target::Command(Found { session, machine })) => {
                let key = BackgroundTask {
                    session,
                    task: ShellTaskId::new(id),
                };
                match machine
                    .background()
                    .stop_background_command(context.operation.clone(), key)
                    .await?
                {
                    BackgroundStop::Stopped { command } => Ok(StoppedTask {
                        id: id.to_owned(),
                        task_type: "local_bash",
                        command,
                        machine: (!machine.is_local()).then(|| machine.name().to_owned()),
                    }),
                    BackgroundStop::NotRunning { status } => Err(ToolError::Failed {
                        message: format!("Task {id} is not running (status: {})", status.as_str()),
                    }),
                    BackgroundStop::NotFound => Err(ToolError::Failed {
                        message: self.not_found(id, context),
                    }),
                }
            }
            Err(message) => Err(ToolError::Failed { message }),
        }
    }
}

/// 回执。缺陷修复（Docker 试用 L10，F98）：停远端机器上的命令时，回执与工具行都看不出停的是哪台上的；现在回执多
/// `machine`，display 带它给 TUI 的工具行（本机不变，参照版本的形状）。
fn json(stopped: &StoppedTask) -> ToolOutput {
    let message = stopped.message();
    let receipt = Stopped {
        message,
        task_id: &stopped.id,
        task_type: stopped.task_type,
        command: &stopped.command,
        machine: stopped.machine.as_deref(),
    };
    let text = serde_json::to_string(&receipt).unwrap_or_default();
    let mut output = text_output(text, false);
    if let Some(machine) = &stopped.machine {
        output.display = serde_json::json!({ "machine": machine });
    }
    output
}

#[async_trait]
impl Tool for TaskStopTool {
    fn name(&self) -> &str {
        NAME
    }

    /// TaskStop 的旧名。
    fn aliases(&self) -> &[&str] {
        &["KillShell", "KillBash"]
    }

    /// 延迟加载（docs/specs/rust-cli/tools-more.md §3.1）。
    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some("kill a running background task")
    }

    fn input_schema(&self) -> Value {
        rust_cli_prompts::tool(NAME).map_or(Value::Null, |prompt| prompt.input_schema.clone())
    }

    fn description(&self, _context: &PromptContext) -> String {
        rust_cli_prompts::tool(NAME).map_or_else(String::new, |prompt| prompt.description.clone())
    }

    fn is_read_only(&self, _input: &Value) -> bool {
        false
    }

    /// TaskStop 可以与别的调用并发。
    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    async fn validate_input(&self, input: &Value, context: &ToolUseContext) -> Validation {
        let invalid = |message: String| Validation::Invalid {
            message,
            error_code: None,
        };
        let Some(id) = task_id(input) else {
            return invalid("Missing required parameter: task_id".to_owned());
        };
        match self.find(&id, &Caller::of(context)).await {
            Ok(_) => Validation::Ok,
            Err(message) => invalid(message),
        }
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
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let Some(id) = task_id(&input) else {
            return Err(ToolError::Failed {
                message: "Missing required parameter: task_id".to_owned(),
            });
        };
        self.stop(&id, &Caller::of(&context))
            .await
            .map(|stopped| json(&stopped))
    }
}

#[cfg(test)]
#[path = "task_stop_tests.rs"]
mod tests;
