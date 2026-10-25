//! Monitor 工具（docs/specs/rust-cli/tools-more.md §19）。
//!
//! 起一条后台命令（或开一个 WebSocket），stdout 的每行是一条事件。工具只做语义：校验、权限的 ws 一半（命令一半交
//! 判决链按 Bash 判）、起后台命令，再经 `Interactions::arm_monitor` 把它交给会话 actor；切行、批、限流、到期与通知都
//! 在 actor 里（session 的 `monitors`）。描述与 schema 是 MO0 基线的 `-p` 写法（`prompts/tools/Monitor.json`），交互
//! 会话与 stream-json 输入的上限换成 30 分钟（[`MonitorTool::with_session_cap`]）。

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_kernel_api::env::{CommandEvent, StartCommand};
use rust_cli_kernel_api::env_shell::{
    self, BackgroundCommand, BackgroundKind, BackgroundTask, CwdScope, MonitorSocket,
    SandboxPolicy, ShellKind,
};
use rust_cli_kernel_api::ids::{OperationId, ShellTaskId};
use rust_cli_kernel_api::monitor::{MonitorArmed, MonitorSource};
use rust_cli_kernel_api::permission::PermissionDecision;
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext, Validation,
};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::common::{Definition, definition, env_failure, text};
use crate::monitor_input::{self, Input};

pub const NAME: &str = "Monitor";

/// 单次的 `-p` 的上限；其余形态 30 分钟。
pub const PRINT_CAP_MS: u64 = 600_000;
pub const SESSION_CAP_MS: u64 = 1_800_000;

const SEARCH_HINT: &str = "watch, monitor, or keep an eye on a process/log/command or WebSocket \u{2014} stream each stdout line as a live notification";

/// 期限的两处写法：描述里的 “at most N minutes” 与 schema 里的 “Deadlines above Nms are capped to Nms.”。
fn cap_texts(cap_ms: u64) -> (String, String) {
    (
        format!("at most {} minutes", cap_ms / 60_000),
        format!("Deadlines above {cap_ms}ms are capped to {cap_ms}ms."),
    )
}

/// 任务 ID：命令同 Bash（`b` 加 8 位），ws 是 `s`。都由 operationId 派生，重放时相同。
fn task_id(operation: &OperationId, socket: bool) -> ShellTaskId {
    let id = crate::bash_output::task_id(operation);
    if !socket {
        return id;
    }
    ShellTaskId::new(format!("s{}", &id.as_str()[1..]))
}

pub struct MonitorTool {
    definition: Definition,
    cap_ms: u64,
    /// 命令来源套的 sandbox 规则（同 Bash 的缺省规则）；None 不套。
    sandbox: Option<SandboxPolicy>,
}

impl Default for MonitorTool {
    fn default() -> Self {
        Self::new()
    }
}

impl MonitorTool {
    /// 单次的 `-p` 的写法（上限 10 分钟，MO0 基线）。
    pub fn new() -> Self {
        Self {
            definition: definition(NAME),
            cap_ms: PRINT_CAP_MS,
            sandbox: None,
        }
    }

    /// 交互会话与 stream-json 输入的 `-p`：上限 30 分钟，描述与 schema 的数字随之换。
    #[must_use]
    pub fn with_session_cap(mut self, session: bool) -> Self {
        let cap = if session {
            SESSION_CAP_MS
        } else {
            PRINT_CAP_MS
        };
        let (old_sentence, old_schema) = cap_texts(self.cap_ms);
        let (sentence, schema) = cap_texts(cap);
        self.definition.description =
            self.definition
                .description
                .replacen(&old_sentence, &sentence, 1);
        if let Some(Value::String(text)) = self
            .definition
            .schema
            .pointer_mut("/properties/timeout_ms/description")
        {
            *text = text.replacen(&old_schema, &schema, 1);
        }
        self.cap_ms = cap;
        self
    }

    #[must_use]
    pub fn with_sandbox(mut self, sandbox: Option<SandboxPolicy>) -> Self {
        self.sandbox = sandbox;
        self
    }
}

/// 等到 `Started`，拿到输出文件。
async fn started(context: &ToolUseContext) -> Result<String, ToolError> {
    let mut events = context
        .machine
        .commands()
        .watch_command(context.operation.operation.clone(), 0);
    while let Some(event) = events.next().await {
        if let CommandEvent::Started { output } = event.map_err(env_failure)? {
            return Ok(output.as_str().to_owned());
        }
    }
    Err(ToolError::Failed {
        message: "the monitor did not start".to_owned(),
    })
}

#[async_trait]
impl Tool for MonitorTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, _context: &PromptContext) -> String {
        self.definition.description.clone()
    }

    fn should_defer(&self) -> bool {
        true
    }

    fn search_hint(&self) -> Option<&str> {
        Some(SEARCH_HINT)
    }

    fn is_concurrency_safe(&self, _input: &Value) -> bool {
        true
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(10_000)
    }

    async fn validate_input(&self, input: &Value, _context: &ToolUseContext) -> Validation {
        match monitor_input::issues(input) {
            Some(issues) => Validation::Invalid {
                message: monitor_input::validation_message(&issues),
                error_code: None,
            },
            None => Validation::Ok,
        }
    }

    /// ws 总是询问（地址在私有段时拒绝）；命令交判决链按 Bash 判（policy 的内置检查）。
    async fn check_permissions(
        &self,
        input: &Value,
        _context: &ToolUseContext,
    ) -> PermissionDecision {
        let Ok(input) = serde_json::from_value::<Input>(input.clone()) else {
            return PermissionDecision::Passthrough;
        };
        let Some(ws) = input.ws else {
            return PermissionDecision::Passthrough;
        };
        if let Some(host) = monitor_input::private_host(&ws.url) {
            return PermissionDecision::Deny {
                message: rust_cli_prompts::monitor::ws_private(&host),
            };
        }
        PermissionDecision::Ask {
            message: rust_cli_prompts::monitor::ws_ask(
                &ws.url,
                ws.protocols.as_deref().unwrap_or_default(),
            ),
            suggestions: Vec::new(),
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|error| ToolError::InvalidInput {
                message: error.to_string(),
            })?;
        let timeout_ms = input.timeout(self.cap_ms);
        let socket = input.ws.as_ref().map(|ws| MonitorSocket {
            url: ws.url.clone(),
            protocols: ws.protocols.clone().unwrap_or_default(),
        });
        let task = task_id(&context.operation.operation, socket.is_some());
        let command = match (&socket, &input.command) {
            (Some(socket), _) => socket.url.clone(),
            (None, Some(command)) => command.clone(),
            (None, None) => {
                return Err(ToolError::InvalidInput {
                    message: "exactly one of command or ws".to_owned(),
                });
            }
        };
        // 起在会话当前目录、结束后不改它；没有执行环境的期限（到期由 actor 管）。
        let cwd = env_shell::current_dir(
            context.machine.shell().as_ref(),
            &context.workspace,
            &context.session,
        )
        .await;
        let start = StartCommand {
            command,
            cwd,
            timeout_ms: None,
            task: task.clone(),
            shell: ShellKind::Bash,
            cwd_scope: CwdScope::Pinned,
            background: Some(BackgroundCommand {
                description: input.description.clone(),
                tool_use: Some(context.tool_use.clone()),
                kind: BackgroundKind::Monitor,
            }),
            sandbox: if socket.is_some() {
                None
            } else {
                self.sandbox.clone()
            },
            hook: None,
            on_timeout: None,
            socket: socket.clone(),
            env: Default::default(),
        };
        context
            .machine
            .commands()
            .start_command(&context.workspace, context.operation.clone(), start)
            .await
            .map_err(env_failure)?;
        let output = started(&context).await?;
        let armed = MonitorArmed {
            // 在远端机器上起的命令：输出流与停止都发给那台机器（R3，remote-exec.md §20.7.2）。
            machine: (!context.machine.is_local()).then(|| context.machine.name().to_owned()),
            operation: context.operation.operation.clone(),
            task: task.clone(),
            tool_use: context.tool_use.clone(),
            description: input.description,
            timeout_ms,
            source: if socket.is_some() {
                MonitorSource::Socket
            } else {
                MonitorSource::Command
            },
            output: rust_cli_kernel_api::env::EnvPath::new(output),
        };
        if let Err(error) = context
            .interactions
            .arm_monitor(&context.session, armed)
            .await
        {
            // 没有会话 owner 接手（不该发生：子代理没有 Monitor）：停掉已经起的命令。
            let request = BackgroundTask {
                session: context.session.clone(),
                task,
            };
            let _ = context
                .machine
                .background()
                .stop_background_command(context.operation.clone(), request)
                .await;
            return Err(ToolError::Failed {
                message: error.to_string(),
            });
        }
        let duration = rust_cli_kernel_api::metering::format_duration_trimmed(timeout_ms);
        Ok(text(rust_cli_prompts::monitor::started(
            task.as_str(),
            &duration,
        )))
    }
}

#[cfg(test)]
#[path = "monitor_tests.rs"]
mod tests;
