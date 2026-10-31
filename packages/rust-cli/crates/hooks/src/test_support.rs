//! hooks 测试共用的替身：按脚本回放命令输出的执行环境、记录请求的 HTTP 出口、引擎构造。

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, Outcome,
    OutputStream, ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_shell::ShellState;
use rust_cli_kernel_api::hooks::{
    HookContext, HookEvent, HookEventName, HookHttp, HookHttpError, HookHttpRequest,
    HookHttpResponse,
};
use rust_cli_kernel_api::ids::{EnvironmentId, Generation, OperationId, SessionId};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};
use rust_cli_kernel_api::testing::{MemoryArtifacts, SequentialIds};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::config::{HookSource, HooksConfig, HooksSettings};
use crate::{HookEngine, HookPorts};

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// 一条命令的脚本：输出、退出码与耗时。`delay_ms` 超过请求的超时时按超时结束。
#[derive(Debug, Clone, Default)]
pub struct Script {
    pub stdout: String,
    pub stderr: String,
    pub exit: i32,
    pub delay_ms: u64,
}

pub fn script(stdout: &str, stderr: &str, exit: i32) -> Script {
    Script {
        stdout: stdout.to_owned(),
        stderr: stderr.to_owned(),
        exit,
        delay_ms: 0,
    }
}

struct Started {
    script: Script,
    timeout_ms: u64,
    cancel: CancellationToken,
}

/// 按命令文本回放脚本的执行环境；没有脚本的命令以退出码 0、无输出结束。
#[derive(Default)]
pub struct ScriptedEnv {
    scripts: Mutex<HashMap<String, Script>>,
    started: Mutex<Vec<(OperationContext, StartCommand)>>,
    running: Mutex<HashMap<OperationId, Started>>,
    shell_cwd: Option<String>,
    fail_start: bool,
}

impl ScriptedEnv {
    pub fn with(scripts: &[(&str, Script)]) -> Self {
        Self {
            scripts: Mutex::new(
                scripts
                    .iter()
                    .map(|(command, script)| ((*command).to_owned(), script.clone()))
                    .collect(),
            ),
            ..Self::default()
        }
    }

    #[must_use]
    pub fn cwd(mut self, cwd: &str) -> Self {
        self.shell_cwd = Some(cwd.to_owned());
        self
    }

    #[must_use]
    pub fn failing() -> Self {
        Self {
            fail_start: true,
            ..Self::default()
        }
    }

    pub fn started(&self) -> Vec<(OperationContext, StartCommand)> {
        lock(&self.started).clone()
    }
}

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "scripted env".to_owned(),
    }
}

#[async_trait]
impl EnvInfo for ScriptedEnv {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Files for ScriptedEnv {
    async fn read_file(&self, _w: &WorkspaceRef, _r: ReadFile) -> Result<FileContent, EnvError> {
        Err(unused())
    }

    async fn write_file(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        _r: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Commands for ScriptedEnv {
    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        let operation = context.operation.clone();
        if self.fail_start {
            return Err(EnvError::Io {
                message: "spawn failed".to_owned(),
            });
        }
        let script = lock(&self.scripts)
            .get(&request.command)
            .cloned()
            .unwrap_or_default();
        lock(&self.running).insert(
            context.operation.clone(),
            Started {
                script,
                timeout_ms: request.timeout_ms.unwrap_or(u64::MAX),
                cancel: CancellationToken::new(),
            },
        );
        lock(&self.started).push((context, request));
        Ok(self.watch_command(operation, 0))
    }

    fn watch_command(
        &self,
        operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        let Some(started) = lock(&self.running).remove(&operation) else {
            return stream::iter([Err(unused())]).boxed();
        };
        let Started {
            script,
            timeout_ms,
            cancel,
        } = started;
        // 取消令牌留给 cancel() 使用。
        lock(&self.running).insert(
            operation,
            Started {
                script: Script::default(),
                timeout_ms,
                cancel: cancel.clone(),
            },
        );
        let run = async move {
            let wait = Duration::from_millis(script.delay_ms.min(timeout_ms));
            let cancelled = tokio::select! {
                () = tokio::time::sleep(wait) => false,
                () = cancel.cancelled() => true,
            };
            let finish = |outcome, exit_code| CommandEvent::Finished {
                outcome,
                exit_code,
                timed_out: false,
                cwd_reset: None,
                output_bytes: 0,
                output_limit: None,
            };
            let mut events = vec![CommandEvent::Started {
                output: EnvPath::new("/tmp/out"),
            }];
            if cancelled {
                events.push(finish(Outcome::Cancelled, Some(137)));
            } else if script.delay_ms > timeout_ms {
                events.push(finish(
                    Outcome::Failed {
                        code: Some("timeout".to_owned()),
                        message: "timed out".to_owned(),
                    },
                    Some(143),
                ));
            } else {
                for (stream, text) in [
                    (OutputStream::Stdout, &script.stdout),
                    (OutputStream::Stderr, &script.stderr),
                ] {
                    if !text.is_empty() {
                        events.push(CommandEvent::Output {
                            stream,
                            data: text.clone().into_bytes(),
                            cursor: 0,
                        });
                    }
                }
                events.push(finish(Outcome::Completed, Some(script.exit)));
            }
            stream::iter(events.into_iter().map(Ok))
        };
        stream::once(run).flatten().boxed()
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        if let Some(started) = lock(&self.running).get(&operation) {
            started.cancel.cancel();
        }
        Ok(())
    }

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

#[async_trait]
impl Shell for ScriptedEnv {
    async fn shell_state(
        &self,
        _workspace: &WorkspaceRef,
        _session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        match &self.shell_cwd {
            Some(cwd) => Ok(ShellState {
                cwd: EnvPath::new(cwd.clone()),
                outputs: None,
                background: Vec::new(),
            }),
            None => Err(unused()),
        }
    }
}

impl Search for ScriptedEnv {}
impl Background for ScriptedEnv {}
impl Checkpoints for ScriptedEnv {}
impl Pdf for ScriptedEnv {}
impl Git for ScriptedEnv {}
impl Fetch for ScriptedEnv {}

/// 记录请求、按脚本回答的 HTTP 出口。
#[derive(Default)]
pub struct ScriptedHttp {
    pub response: Mutex<Option<Result<HookHttpResponse, HookHttpError>>>,
    pub requests: Mutex<Vec<HookHttpRequest>>,
}

impl ScriptedHttp {
    pub fn answering(status: u16, body: &str) -> Self {
        Self {
            response: Mutex::new(Some(Ok(HookHttpResponse {
                status,
                body: body.to_owned(),
            }))),
            requests: Mutex::default(),
        }
    }
}

#[async_trait]
impl HookHttp for ScriptedHttp {
    async fn post(
        &self,
        request: HookHttpRequest,
        _cancel: CancellationToken,
    ) -> Result<HookHttpResponse, HookHttpError> {
        lock(&self.requests).push(request);
        lock(&self.response)
            .clone()
            .unwrap_or(Err(HookHttpError::Failed {
                message: "no response".to_owned(),
            }))
    }
}

pub fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    }
}

pub fn ports<E: Commands + Shell + 'static>(env: Arc<E>) -> HookPorts {
    HookPorts {
        commands: env.clone(),
        shell: env,
        ids: Arc::new(SequentialIds::default()),
        model: None,
        http: None,
        generation: Generation::new("g1"),
        artifacts: Arc::new(MemoryArtifacts::default()),
    }
}

/// 单一来源（用户配置）的快照。
pub fn config(settings: &Value) -> HooksConfig {
    let (parsed, warnings) = HooksSettings::from_json(settings);
    assert!(warnings.is_empty(), "{warnings:?}");
    HooksConfig {
        version: 1,
        sources: vec![(HookSource::User, parsed)],
        ..HooksConfig::default()
    }
}

pub fn engine<E: Commands + Shell + 'static>(env: Arc<E>, settings: &Value) -> HookEngine {
    HookEngine::with_config(ports(env), config(settings))
}

pub fn event(name: HookEventName, payload: Value) -> HookEvent {
    HookEvent {
        name,
        session: SessionId::new("sess_1"),
        workspace: workspace(),
        payload,
        context: HookContext::default(),
    }
}

/// 一个事件只配一个 command hook 的设置。
pub fn command_settings(event: &str, matcher: &str, command: &str) -> Value {
    serde_json::json!({
        "hooks": {event: [{"matcher": matcher, "hooks": [{"type": "command", "command": command}]}]}
    })
}
