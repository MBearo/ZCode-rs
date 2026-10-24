//! Bash 工具（shell-permissions.md §1）。
//!
//! 语义在这里，进程在执行环境里：`start_command` 起命令（stdout 与 stderr 合流写进输出文件），
//! `watch_command` 按游标读输出直到终态。前台命令在本次调用里等结果；后台命令拿到输出文件就返回，
//! 终态以通知送达。超时默认 2 分钟、前台最多 10 分钟，后台默认 10 分钟、最多 2 小时。主线程的 cwd
//! 在调用之间延续（越出工作区时重置），子代理固定在当前 cwd。中断：调用被取消时向执行环境发取消，
//! 等到终态再返回 `Cancelled`。权限由判决链（`rust-cli-policy`）判，工具自身不做检查。

use async_trait::async_trait;
use futures_util::StreamExt;
use rust_cli_kernel_api::env::{CommandEvent, EnvPath, Outcome, StartCommand};
use rust_cli_kernel_api::env_shell::{
    self, BackgroundCommand, CwdScope, SandboxPolicy, ShellKind, TimeoutBackground,
};
use rust_cli_kernel_api::tool::{
    PromptContext, Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext,
};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::bash_output::{self as output, INLINE_LIMIT};
pub use crate::bash_timeouts::BashTimeouts;
use crate::common::{Definition, definition, env_failure, text};

pub const NAME: &str = "Bash";

// 缺陷修复：Bash 提示原来总用缺省的文字。启动参数点名了
// Glob / Grep（它们在工具清单里）时提示另点名 `find`、`grep`；缺省不点名。工具清单就是唯一来源：
// 这两个工具可见就用点名的写法。
const WITHOUT_SEARCH_TOOLS: &str = "`cat`, `head`, `tail`, `sed`, `awk`, or `echo`";
const WITH_SEARCH_TOOLS: &str = "`find`, `grep`, `cat`, `head`, `tail`, `sed`, `awk`, or `echo`";

// 修 bug（修复线 F9）：只在非交互会话（`-p`）给后台命令设期限（shell-permissions.md §1.1）；
// 原来 TUI 也照 `-p` 写期限、设超时。交互会话去掉期限的两段说明（`BashTimeouts` 的
// `deadline_sentence` / `deadline_parameter`），后台命令不设超时。
/// “不要用 cat”一行的开头（只在没有去掉这一行时出现；两种点名写法开头相同）。
const AVOID_LINE: &str = "\n- IMPORTANT: Avoid using this tool to run ";

/// 去掉以 `start`（含前面的换行）开头的那一行；没有这一行时原样返回。
fn without_line(text: &str, start: &str) -> String {
    let Some(at) = text.find(start) else {
        return text.to_owned();
    };
    let end = text[at + 1..]
        .find('\n')
        .map_or(text.len(), |offset| at + 1 + offset);
    format!("{}{}", &text[..at], &text[end..])
}

#[derive(Debug, Deserialize)]
struct Input {
    command: String,
    #[serde(default)]
    timeout: Option<f64>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    run_in_background: Option<bool>,
    #[serde(default, rename = "dangerouslyDisableSandbox")]
    dangerously_disable_sandbox: Option<bool>,
}

/// 最后一个子命令的命令名（退出码解释用）。
fn last_command(command: &str) -> String {
    let analysis = rust_cli_policy::analyze(command);
    analysis
        .commands
        .last()
        .map(|invocation| invocation.name.clone())
        .or_else(|| command.split_whitespace().next().map(str::to_owned))
        .unwrap_or_default()
}

/// 前台命令到了超时能不能转到后台（shell-permissions.md §1.2.1）：第一个子命令的第一个词不是 `sleep` 就能；拆不出
/// 子命令时也能。
fn moves_on_timeout(command: &str) -> bool {
    let analysis = rust_cli_policy::analyze(command);
    let Some(first) = analysis.commands.first() else {
        return true;
    };
    first.command_text.split_whitespace().next() != Some("sleep")
}

pub struct BashTool {
    definition: Definition,
    /// 默认的 sandbox 规则；None 表示不套 sandbox（默认 `sandbox.enabled = false`）。
    sandbox: Option<SandboxPolicy>,
    /// 后台命令有没有期限：`-p` 有（缺省），交互会话没有（[`BashTool::with_background_deadline`]）。
    background_deadline: bool,
    /// 前台与后台的缺省与上限（入口按 `BASH_*_TIMEOUT_MS` 给出，[`BashTool::with_timeouts`]）。
    timeouts: BashTimeouts,
    /// 读文件的命令记进的 readFileState（tools-files.md §4.2）；None 时不记。
    files: Option<std::sync::Arc<crate::file_state::ReadFileState>>,
    /// Monitor 开着（工作线 F83，tools-more.md §19.9）：描述多一句，前台的长 `sleep` 拦下。
    monitor: bool,
}

impl Default for BashTool {
    fn default() -> Self {
        Self::new()
    }
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            definition: definition(NAME),
            sandbox: None,
            background_deadline: true,
            timeouts: BashTimeouts::default(),
            files: None,
            monitor: false,
        }
    }

    /// Monitor 开着（工作线 F83，tools-more.md §19.9）：`run_in_background` 一行末尾加 “Foreground `sleep` is
    /// blocked; …” 一句，没有 `run_in_background` 的前台长 `sleep` 拦下（errorCode 10）。与期限的两个方法先后无关。
    #[must_use]
    pub fn with_monitor(mut self, enabled: bool) -> Self {
        if enabled && !self.monitor {
            const ANCHOR: &str = "No `&` needed.";
            self.definition.description = self.definition.description.replacen(
                ANCHOR,
                &format!("{ANCHOR}{}", rust_cli_prompts::monitor::bash_sentence()),
                1,
            );
        }
        self.monitor = enabled;
        self
    }

    /// 读文件的命令（`cat`、`head`、`sed -n` 等）正常结束后把文件记进这份 readFileState（文件工具用的同一份，
    /// tools-files.md §4.2）。
    #[must_use]
    pub fn with_file_state(
        mut self,
        files: std::sync::Arc<crate::file_state::ReadFileState>,
    ) -> Self {
        self.files = Some(files);
        self
    }

    /// stream-json 输入的 `-p`：后台命令的缺省期限是 max(1800000, 前台
    /// 缺省)，描述与 schema 里的数字随之变（headless.md §6.1，修复线 F39；F42 起经 [`BashTimeouts`]，
    /// 与 `BASH_DEFAULT_TIMEOUT_MS` 一起算）。
    #[must_use]
    pub fn with_streaming_input(self) -> Self {
        let timeouts = self.timeouts.with_streaming_input();
        self.with_timeouts(timeouts)
    }

    /// 后台命令的期限：交互会话传 false，描述与 schema 去掉期限的说明，后台命令不设超时。
    #[must_use]
    pub fn with_background_deadline(mut self, enabled: bool) -> Self {
        if !enabled {
            let (sentence, parameter) = (
                self.timeouts.deadline_sentence(),
                self.timeouts.deadline_parameter(),
            );
            self.definition.description = self.definition.description.replacen(&sentence, "", 1);
            if let Some(Value::String(text)) = self
                .definition
                .schema
                .pointer_mut("/properties/run_in_background/description")
            {
                *text = text.replacen(&parameter, "", 1);
            }
        }
        self.background_deadline = enabled;
        self
    }

    /// 进程启动时是 bypass 类别时去掉 `- IMPORTANT: Avoid using this tool to run …` 一行（修复线 F43，
    /// shell-permissions.md §1.1、§4.9）。条件是自动模式或会话的 bypass 模式，
    /// 会话里算一次就不变，所以按启动时的模式定下，不看每一步的模式；工具定义在会话中途不变，缓存前缀不断。
    #[must_use]
    pub fn with_bash_first_trimmed(mut self, trimmed: bool) -> Self {
        if trimmed {
            self.definition.description = without_line(&self.definition.description, AVOID_LINE);
        }
        self
    }

    /// 超时的缺省与上限（`BASH_DEFAULT_TIMEOUT_MS` / `BASH_MAX_TIMEOUT_MS`，cli-args.md §9.3，修复线 F42）：执行时用
    /// 它们，描述与 schema 里的数字跟着换（文字由这些值生成）。与 [`BashTool::with_background_deadline`] 的
    /// 先后不影响结果。
    #[must_use]
    pub fn with_timeouts(mut self, timeouts: BashTimeouts) -> Self {
        let old = self.timeouts;
        self.definition.description = self
            .definition
            .description
            .replacen(&old.foreground_line(), &timeouts.foreground_line(), 1)
            .replacen(&old.deadline_sentence(), &timeouts.deadline_sentence(), 1);
        let schema = &mut self.definition.schema;
        if let Some(Value::String(text)) = schema.pointer_mut("/properties/timeout/description") {
            *text = text.replacen(&old.timeout_parameter(), &timeouts.timeout_parameter(), 1);
        }
        if let Some(Value::String(text)) =
            schema.pointer_mut("/properties/run_in_background/description")
        {
            *text = text.replacen(&old.deadline_parameter(), &timeouts.deadline_parameter(), 1);
        }
        self.timeouts = timeouts;
        self
    }

    /// 开启 sandbox：命令默认在这些规则下执行，`dangerouslyDisableSandbox` 时不套。
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: SandboxPolicy) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    fn sandbox_for(&self, input: &Input) -> Option<SandboxPolicy> {
        if input.dangerously_disable_sandbox == Some(true) {
            return None;
        }
        self.sandbox.clone()
    }
}

/// 读事件流直到终态。返回输出文件、前 [`INLINE_LIMIT`] 字节与终态事件；被取消时先发取消再等终态。
struct Collected {
    output_file: String,
    head: Vec<u8>,
    finished: CommandEvent,
}

async fn collect(
    context: &ToolUseContext,
    progress: &mpsc::Sender<ToolProgress>,
    task: &str,
    timeout_ms: u64,
) -> Result<Collected, ToolError> {
    let operation = context.operation.operation.clone();
    let mut events = context
        .machine
        .commands()
        .watch_command(operation.clone(), 0);
    let mut output_file = String::new();
    let mut head = Vec::new();
    let mut total: u64 = 0;
    let mut cancelled = false;
    loop {
        let event = tokio::select! {
            event = events.next() => event,
            () = context.cancel.cancelled(), if !cancelled => {
                cancelled = true;
                context.machine.commands().cancel(operation.clone()).await.map_err(env_failure)?;
                continue;
            }
        };
        let Some(event) = event else {
            return Err(ToolError::Failed {
                message: "the command output stream ended without a result".to_owned(),
            });
        };
        match event.map_err(env_failure)? {
            CommandEvent::Started { output } => output_file = output.as_str().to_owned(),
            CommandEvent::Output { data, .. } => {
                total += data.len() as u64;
                let room = INLINE_LIMIT.saturating_sub(head.len());
                head.extend_from_slice(&data[..data.len().min(room)]);
                let tail =
                    String::from_utf8_lossy(&data[data.len().saturating_sub(2_000)..]).into_owned();
                let _ = progress.try_send(ToolProgress {
                    data: json!({
                        "type": "bash_progress",
                        "output": tail,
                        "totalBytes": total,
                        "taskId": task,
                        "timeoutMs": timeout_ms,
                    }),
                });
            }
            // 到了超时、转到了后台：结果是转后台的说明，之后的输出与终态不再等（取消已发出时照旧等终态）。
            moved @ CommandEvent::Backgrounded if !cancelled => {
                return Ok(Collected {
                    output_file,
                    head,
                    finished: moved,
                });
            }
            CommandEvent::Backgrounded => {}
            finished @ CommandEvent::Finished { .. } => {
                if cancelled {
                    return Err(ToolError::Cancelled);
                }
                return Ok(Collected {
                    output_file,
                    head,
                    finished,
                });
            }
        }
    }
}

/// 后台命令：只等 `Started` 拿到输出文件。
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
        message: "the background command did not start".to_owned(),
    })
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        NAME
    }

    fn input_schema(&self) -> Value {
        self.definition.schema.clone()
    }

    fn description(&self, context: &PromptContext) -> String {
        let search_tools = context
            .tool_names
            .iter()
            .any(|name| name == crate::glob::NAME || name == crate::grep::NAME);
        if search_tools {
            self.definition
                .description
                .replacen(WITHOUT_SEARCH_TOOLS, WITH_SEARCH_TOOLS, 1)
        } else {
            self.definition.description.clone()
        }
    }

    fn is_read_only(&self, input: &Value) -> bool {
        input
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| rust_cli_policy::readonly::is_read_only(command, false))
    }

    fn is_concurrency_safe(&self, input: &Value) -> bool {
        self.is_read_only(input)
    }

    fn max_result_size_chars(&self) -> Option<usize> {
        Some(INLINE_LIMIT)
    }

    async fn validate_input(
        &self,
        input: &Value,
        _context: &ToolUseContext,
    ) -> rust_cli_kernel_api::tool::Validation {
        use rust_cli_kernel_api::tool::Validation;
        if !self.monitor || input.get("run_in_background").and_then(Value::as_bool) == Some(true) {
            return Validation::Ok;
        }
        let command = input
            .get("command")
            .and_then(Value::as_str)
            .unwrap_or_default();
        match crate::bash_sleep::blocked(command) {
            Some(what) => Validation::Invalid {
                message: rust_cli_prompts::monitor::bash_blocked(&what),
                error_code: Some(10),
            },
            None => Validation::Ok,
        }
    }

    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|error| ToolError::InvalidInput {
                message: error.to_string(),
            })?;
        match self.run(input, context, progress, true).await? {
            Ran::Text(body) => Ok(text(body)),
            Ran::Failed { code, output } => Err(ToolError::Failed {
                message: output::failure(code, &output),
            }),
        }
    }
}

/// 一次执行的结果（[`Tool::call`] 与 bash 模式共用，docs/specs/rust-cli/tui-input.md §2.2）。
pub(crate) enum Ran {
    /// 结果的文字：成功的输出（超长时是 `<persisted-output>` 预览）或后台命令的启动说明。
    Text(String),
    /// 退出码表示失败或超时：合流的输出（超时时前面是超时说明），不带 `Exit code N`。
    Failed { code: i32, output: String },
}

impl BashTool {
    /// bash 模式（用户在输入框里 `!命令`）：不套 sandbox（`dangerouslyDisableSandbox: true`），
    /// 前台、默认超时，同一个 shell 状态与 cwd。
    pub(crate) async fn run_typed(
        &self,
        command: &str,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
    ) -> Result<Ran, ToolError> {
        let input = Input {
            command: command.to_owned(),
            timeout: None,
            description: None,
            run_in_background: None,
            dangerously_disable_sandbox: Some(true),
        };
        // 用户在输入框里的命令到了超时照旧结束（不转后台）。
        self.run(input, context, progress, false).await
    }

    /// `may_move`：前台命令到了超时可以转到后台（模型的调用；shell-permissions.md §1.2.1）。
    async fn run(
        &self,
        input: Input,
        context: ToolUseContext,
        progress: mpsc::Sender<ToolProgress>,
        may_move: bool,
    ) -> Result<Ran, ToolError> {
        let background = input.run_in_background == Some(true);
        let timeout_ms = self.timeouts.effective(input.timeout, background);
        let description = input
            .description
            .clone()
            .unwrap_or_else(|| input.command.clone());
        // 修 bug（工作线 F69）：以前前台命令到了超时一律结束，长编译、长测试被杀。现在请执行环境转到后台继续跑，
        // 之后的期限是后台命令的缺省期限（交互会话没有）；第一个词是 `sleep` 的命令照旧结束。
        let on_timeout = (may_move && !background && moves_on_timeout(&input.command)).then(|| {
            TimeoutBackground {
                command: BackgroundCommand {
                    description: description.clone(),
                    tool_use: Some(context.tool_use.clone()),
                    kind: Default::default(),
                },
                deadline_ms: self
                    .background_deadline
                    .then(|| self.timeouts.effective(None, true)),
            }
        });
        let deadline_ms = on_timeout.as_ref().and_then(|moved| moved.deadline_ms);
        // 交互会话的后台命令没有期限。
        let start_timeout = (!background || self.background_deadline).then_some(timeout_ms);
        let task = output::task_id(&context.operation.operation);
        let workspace = &context.workspace;
        // 子代理不改主线程的 cwd：在子会话的当前目录启动（派生时取自父会话，
        // tools-files.md §3.3）、结束后不保留。
        // 修复线 F68（审计 T3）：主线程的后台命令也这样。以前后台命令用会话范围，结束时把 cwd 写回会话；后台命令
        // 什么时候结束不确定，会话的当前目录会在之后任意两个工具调用之间跳走（竞态）。
        let (cwd, cwd_scope) = if context.agent.is_some() || background {
            let current = env_shell::current_dir(
                context.machine.shell().as_ref(),
                workspace,
                &context.session,
            )
            .await;
            (current, CwdScope::Pinned)
        } else {
            let roots = vec![workspace.root.clone()];
            (workspace.root.clone(), CwdScope::Session { roots })
        };
        let start = StartCommand {
            command: input.command.clone(),
            cwd: cwd.clone(),
            timeout_ms: start_timeout,
            task: task.clone(),
            shell: ShellKind::Bash,
            cwd_scope,
            background: background.then(|| BackgroundCommand {
                description,
                tool_use: Some(context.tool_use.clone()),
                kind: Default::default(),
            }),
            sandbox: self.sandbox_for(&input),
            hook: None,
            on_timeout,
            socket: None,
        };
        context
            .machine
            .commands()
            .start_command(workspace, context.operation.clone(), start)
            .await
            .map_err(env_failure)?;
        if background {
            let file = started(&context).await?;
            let mut text = output::backgrounded(&task, &file);
            if changes_directory(&input.command) {
                text.push('\n');
                text.push_str(&output::background_cwd_hint(cwd.as_str()));
            }
            return Ok(Ran::Text(text));
        }
        let collected = collect(&context, &progress, task.as_str(), timeout_ms).await?;
        if collected.finished == CommandEvent::Backgrounded {
            return Ok(Ran::Text(output::moved_to_background(
                timeout_ms,
                &task,
                &collected.output_file,
                deadline_ms,
            )));
        }
        let CommandEvent::Finished {
            outcome,
            exit_code,
            timed_out,
            cwd_reset,
            output_bytes,
            output_limit,
        } = collected.finished
        else {
            return Err(ToolError::Failed {
                message: "unexpected command event".to_owned(),
            });
        };
        let head = String::from_utf8_lossy(&collected.head).into_owned();
        if let Outcome::Failed { message, .. } = &outcome
            && !timed_out
        {
            return Err(ToolError::Failed {
                message: message.clone(),
            });
        }
        // 缺陷修复（审查 R2-18）：执行环境因输出文件超过上限结束了命令（终态也是 `Cancelled`），不是用户取消。
        // 说明放在输出前、退出码 137 按错误报告（shell-permissions.md §1.3、§2.2）。
        if let Some(note) = output_limit {
            return Ok(Ran::Failed {
                code: exit_code.unwrap_or(137),
                output: output::killed_for_size(&note, &head),
            });
        }
        if matches!(outcome, Outcome::Cancelled | Outcome::Interrupted) {
            return Err(ToolError::Cancelled);
        }
        let head = if timed_out {
            output::timed_out(timeout_ms, &head)
        } else {
            head
        };
        let code = exit_code.unwrap_or(1);
        if timed_out || output::is_error(&last_command(&input.command), code) {
            return Ok(Ran::Failed { code, output: head });
        }
        let reset = cwd_reset.as_ref().map(EnvPath::as_str);
        let total = output_bytes.max(head.len() as u64);
        // 读文件的命令：记进 readFileState（输出太长、给的是预览时，记录不算在上下文里）。
        if let Some(files) = &self.files {
            let truncated = total > INLINE_LIMIT as u64;
            crate::bash_reads::record(files, &context, &input.command, (code, truncated)).await;
        }
        Ok(Ran::Text(output::success(
            &head,
            total,
            &collected.output_file,
            reset,
        )))
    }
}

/// 命令的任一子命令去掉包装（`timeout`、`nohup`、安全的环境变量等）后第一个词是 `cd`、`pushd`、`popd` 或 `chdir`。
/// 解析不了时按 `&&`、`||`、`;`、`|`、换行粗拆。
fn changes_directory(command: &str) -> bool {
    const NAMES: [&str; 4] = ["cd", "pushd", "popd", "chdir"];
    let analysis = rust_cli_policy::analyze(command);
    let parts: Vec<String> = if analysis.has_parse_errors
        || analysis.has_unsupported_syntax
        || analysis.commands.is_empty()
    {
        command
            .split(['&', '|', ';', '\n'])
            .map(|part| part.trim_start().trim_start_matches(['(', '{']).to_owned())
            .collect()
    } else {
        analysis
            .commands
            .into_iter()
            .map(|invocation| invocation.command_text)
            .collect()
    };
    parts.iter().any(|part| {
        rust_cli_policy::shell_rules::strip_safe_wrappers(part)
            .split_whitespace()
            .next()
            .is_some_and(|word| NAMES.contains(&word))
    })
}

#[cfg(test)]
#[path = "bash_tests.rs"]
mod tests;
