//! command hook：经执行环境的 `start_command` 运行（docs/specs/rust-cli/hooks.md §5.1）。
//!
//! 输入 JSON 加换行写进
//! stdin，stdout 与 stderr 分开收集。给 hook 的变量按 Node（`createPluginEnvOverlay`）两套名字都给：项目目录
//! `ZCODE_PROJECT_DIR` / `CLAUDE_PROJECT_DIR`、会话 ID（修复线 F42，config.md §2.5.1）。
//! 进程、超时与进程树回收都在执行环境里；这里只发请求、读事件。取消是一条消息，发出后继续等终态。

use std::collections::BTreeMap;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvPath, OperationContext, Origin, Outcome, OutputStream, StartCommand,
    WorkspaceRef,
};
use rust_cli_kernel_api::env_shell::{CwdScope, HookIo, ShellKind};
use rust_cli_kernel_api::ids::{OperationId, ShellTaskId};
use rust_cli_kernel_api::machine::Commands;
use tokio_util::sync::CancellationToken;

use rust_cli_base::names::vars;

use crate::config::{CommandHook, HookShell};
use crate::result::Raw;

/// 项目目录的环境变量名（两套名字都给，同 Node）。
const PROJECT_DIR_VARS: [&str; 2] = [vars::ZCODE_PROJECT_DIR, vars::CLAUDE_PROJECT_DIR];
/// 会话 ID 的环境变量名（Node `createPluginEnvOverlay` 的三个名字）。
const SESSION_ID_VARS: [&str; 3] = [
    vars::ZCODE_SESSION_ID,
    vars::CLAUDE_SESSION_ID,
    vars::CLAUDE_CODE_SESSION_ID,
];

/// stdout 与 stderr 在内存里合计最多留这么多字节（hooks.md §5.1、D16）：与后台任务输出的内存上限相同。
pub(crate) const MEMORY_LIMIT: usize = 8 * 1024 * 1024;

/// 读到的输出。缺陷修复（审查 R2-18）：原来 stdout / stderr 不设上限地攒在内存里，输出巨大的 hook 能把进程的内存
/// 吃光。现在合计最多 [`MEMORY_LIMIT`]，先到先留，超出的丢掉（只计数，结束时记一条 warn）。
#[derive(Default)]
struct Captured {
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    dropped: u64,
}

impl Captured {
    fn push(&mut self, stream: OutputStream, data: &[u8]) {
        let room = MEMORY_LIMIT.saturating_sub(self.stdout.len() + self.stderr.len());
        let kept = data.len().min(room);
        let target = match stream {
            OutputStream::Stdout => &mut self.stdout,
            OutputStream::Stderr => &mut self.stderr,
        };
        target.extend_from_slice(&data[..kept]);
        self.dropped += (data.len() - kept) as u64;
    }
}

/// hook 命令的任务 ID：`h` + operationId 的 FNV-1a 的 8 位 36 进制（与 Bash 的 `b` 前缀区分；base 的 `hash::short_id`）。
pub(crate) fn task_id(operation: &OperationId) -> ShellTaskId {
    ShellTaskId::new(format!(
        "h{}",
        rust_cli_base::hash::short_id(operation.as_str())
    ))
}

fn single_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

/// 插件目录的环境变量（`CLAUDE_PLUGIN_ROOT` 与 `ZCODE_PLUGIN_ROOT` 两套名字都给，与 ext 的 MCP 配置变量相同）。
const PLUGIN_ROOT_VARS: [&str; 2] = [vars::CLAUDE_PLUGIN_ROOT, vars::ZCODE_PLUGIN_ROOT];

/// 插件的 hook：命令里的插件目录变量先换成插件目录。
fn plugin_expand(text: &str, hook: &CommandHook) -> String {
    match &hook.plugin_root {
        Some(root) => PLUGIN_ROOT_VARS.iter().fold(text.to_owned(), |text, var| {
            text.replace(&format!("${{{var}}}"), root)
        }),
        None => text.to_owned(),
    }
}

/// shell 形式原样交给 shell（变量由 shell 从 [`hook_env`] 的环境展开）；exec 形式（`args`）每一项加单引号，参数里的
/// 项目目录与会话 ID 变量先替换（Node `expandPluginVariables`；会话 ID 是 F42 加的）。插件的 hook 在两种形式里都先
/// 替换插件目录变量。
pub(crate) fn command_line(hook: &CommandHook, project_dir: &str, session: Option<&str>) -> String {
    let Some(args) = &hook.args else {
        return plugin_expand(&hook.command, hook);
    };
    let expand = |text: &str| {
        let text = PROJECT_DIR_VARS
            .iter()
            .fold(plugin_expand(text, hook), |text, var| {
                text.replace(&format!("${{{var}}}"), project_dir)
            });
        match session {
            Some(session) => SESSION_ID_VARS.iter().fold(text, |text, var| {
                text.replace(&format!("${{{var}}}"), session)
            }),
            None => text,
        }
    };
    std::iter::once(&hook.command)
        .chain(args)
        .map(|part| single_quote(&expand(part)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// hook 进程额外的环境变量：项目目录与会话 ID（两套名字，按 Node）；插件的 hook 另有插件目录。
///
/// 修复线 F42：以前只有 `ZCODE_PROJECT_DIR`，插件读 `$CLAUDE_PROJECT_DIR` 时是空的；Node 两套都给。
fn hook_env(
    hook: &CommandHook,
    project_dir: &str,
    session: Option<&str>,
) -> BTreeMap<String, String> {
    let mut env: BTreeMap<String, String> = PROJECT_DIR_VARS
        .iter()
        .map(|var| ((*var).to_owned(), project_dir.to_owned()))
        .collect();
    if let Some(session) = session {
        for var in SESSION_ID_VARS {
            env.insert(var.to_owned(), session.to_owned());
        }
    }
    if let Some(root) = &hook.plugin_root {
        for var in PLUGIN_ROOT_VARS {
            env.insert(var.to_owned(), root.clone());
        }
    }
    env
}

pub(crate) struct CommandRun<'a> {
    pub commands: &'a dyn Commands,
    pub workspace: &'a WorkspaceRef,
    pub operation: OperationContext,
    pub hook: &'a CommandHook,
    pub input: &'a str,
    pub timeout_ms: u64,
}

pub(crate) async fn run(run: CommandRun<'_>, cancel: CancellationToken) -> Raw {
    let project_dir = run.workspace.root.as_str().to_owned();
    let operation = run.operation.operation.clone();
    let session = match &run.operation.origin {
        Origin::Agent { session, .. } => Some(session.as_str().to_owned()),
        Origin::User { session } => session.as_ref().map(|session| session.as_str().to_owned()),
    };
    let request = StartCommand {
        command: command_line(run.hook, &project_dir, session.as_deref()),
        cwd: EnvPath::new(project_dir.clone()),
        timeout_ms: Some(run.timeout_ms),
        task: task_id(&operation),
        shell: match run.hook.shell {
            HookShell::Bash => ShellKind::Bash,
            HookShell::PowerShell => ShellKind::PowerShell,
        },
        cwd_scope: CwdScope::Pinned,
        background: None,
        sandbox: None,
        hook: Some(HookIo {
            stdin: format!("{}\n", run.input),
            env: hook_env(run.hook, &project_dir, session.as_deref()),
        }),
        // hook 命令到了超时照旧结束（不转后台）。
        on_timeout: None,
        socket: None,
        env: Default::default(),
    };
    let mut events = match run
        .commands
        .start_command(run.workspace, run.operation, request)
        .await
    {
        Ok(events) => events,
        Err(error) => return Raw::Failed(format!("Failed to run: {error}")),
    };
    if run.hook.background {
        // async：不等待，结果不回送（D6）；丢掉流不影响命令。
        return Raw::Backgrounded;
    }
    let mut captured = Captured::default();
    let mut cancel_sent = false;
    loop {
        let event = tokio::select! {
            event = events.next() => event,
            () = cancel.cancelled(), if !cancel_sent => {
                cancel_sent = true;
                let _ = run.commands.cancel(operation.clone()).await;
                continue;
            }
        };
        match event {
            // hook 命令不设 `on_timeout`，不会转到后台。
            Some(Ok(
                CommandEvent::Started { .. }
                | CommandEvent::Backgrounded
                | CommandEvent::BackgroundedBy { .. },
            )) => {}
            Some(Ok(CommandEvent::Output { stream, data, .. })) => captured.push(stream, &data),
            Some(Ok(CommandEvent::Finished {
                outcome, exit_code, ..
            })) => {
                if captured.dropped > 0 {
                    tracing::warn!(
                        dropped = captured.dropped,
                        limit = MEMORY_LIMIT,
                        "hook output past the memory limit was dropped"
                    );
                }
                return finished(
                    outcome,
                    exit_code,
                    cancel_sent,
                    &captured.stdout,
                    &captured.stderr,
                );
            }
            Some(Err(error)) => return Raw::Failed(format!("Failed to run: {error}")),
            None => return Raw::Failed("Failed to run: command output ended early".to_owned()),
        }
    }
}

fn finished(
    outcome: Outcome,
    exit_code: Option<i32>,
    cancel_sent: bool,
    stdout: &[u8],
    stderr: &[u8],
) -> Raw {
    match outcome {
        Outcome::Completed if !cancel_sent => Raw::Command {
            stdout: String::from_utf8_lossy(stdout).into_owned(),
            stderr: String::from_utf8_lossy(stderr).into_owned(),
            exit: exit_code.unwrap_or(1),
        },
        // 超时与取消都按 cancelled（超时另记一种，`onError: "deny"` 要区分）。
        Outcome::Completed | Outcome::Cancelled | Outcome::Interrupted => Raw::Cancelled,
        Outcome::Failed {
            code: Some(code), ..
        } if code == "timeout" => Raw::TimedOut,
        Outcome::Failed { message, .. } | Outcome::Rejected { message } => {
            Raw::Failed(format!("Failed to run: {message}"))
        }
        Outcome::Running | Outcome::Unknown => {
            Raw::Failed("Failed to run: command result is unknown".to_owned())
        }
    }
}

#[cfg(test)]
#[path = "command_tests.rs"]
mod tests;
