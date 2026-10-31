//! 执行环境的替身：账本里有几条在跑的后台命令（shell 与两种来源的 monitor），其余操作不支持。后台任务 pill 的伪终端
//! 测试（修复线 F117，`background_pty.rs`）与回合结束那一行的测试（修复线 F118，`turn_duration.rs`、
//! `turn_duration_pty.rs`）共用。
use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, FileVersion, OperationContext,
    ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::{ReadText, TextRead, TextSlice};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundKind, BackgroundStatus, BackgroundStop, BackgroundTask, ShellState,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId, ShellTaskId};
use rust_cli_kernel_api::local_services::{Fetch, Git};
use rust_cli_kernel_api::machine::{
    Background, Checkpoints, Commands, EnvInfo, Files, Pdf, Search, Shell,
};

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "test".to_owned(),
    }
}

/// 账本里在跑的后台命令（给出的任务 ID，按 [`known`]）：`b1` 是 shell，`b2` 是命令来源的 monitor，`s3` 是 WebSocket
/// 来源的 monitor。
pub struct Ledger(pub &'static [&'static str]);

/// 任务 → （命令，种类，启动时的说明）。
fn known(task: &str) -> Option<(&'static str, BackgroundKind, &'static str)> {
    Some(match task {
        "b1" => ("npm test", BackgroundKind::Bash, "Run the tests"),
        "b2" => (
            "tail -f build.log | grep --line-buffered ERROR",
            BackgroundKind::Monitor,
            "build log",
        ),
        "s3" => (
            "wss://events.example.com/feed",
            BackgroundKind::Monitor,
            "deploy feed",
        ),
        _ => return None,
    })
}

#[async_trait]
impl EnvInfo for Ledger {
    async fn facts(&self, _w: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Err(unused())
    }
}

#[async_trait]
impl Files for Ledger {
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

    async fn read_text(&self, _w: &WorkspaceRef, _r: ReadText) -> Result<TextRead, EnvError> {
        Ok(TextRead::Text(TextSlice {
            content: "ERROR one".to_owned(),
            line_count: 1,
            total_lines: 1,
            total_bytes: 0,
            read_bytes: 0,
            mtime_ms: 0,
            version: FileVersion::new("v1"),
        }))
    }
}

#[async_trait]
impl Commands for Ledger {
    async fn start_command(
        &self,
        _w: &WorkspaceRef,
        _c: OperationContext,
        _r: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        Err(unused())
    }

    fn watch_command(
        &self,
        _o: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        stream::iter([Err(unused())]).boxed()
    }

    async fn cancel(&self, _o: OperationId) -> Result<(), EnvError> {
        Err(unused())
    }

    async fn close_session(&self, _s: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

#[async_trait]
impl Shell for Ledger {
    async fn shell_state(&self, _w: &WorkspaceRef, _s: &SessionId) -> Result<ShellState, EnvError> {
        Ok(ShellState {
            cwd: EnvPath::new("/Users/dev/project"),
            outputs: Some(EnvPath::new("/Users/dev/tasks")),
            background: self.0.iter().copied().map(ShellTaskId::new).collect(),
        })
    }
}

#[async_trait]
impl Background for Ledger {
    async fn background_command(
        &self,
        task: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        let listed = self.0.contains(&task.task.as_str());
        Ok(known(task.task.as_str())
            .filter(|_| listed)
            .map(|(command, kind, description)| BackgroundInfo {
                command: command.to_owned(),
                status: BackgroundStatus::Running,
                kind,
                description: description.to_owned(),
            }))
    }

    async fn background_elapsed(&self, _task: BackgroundTask) -> Result<Option<u64>, EnvError> {
        Ok(Some(5_000))
    }

    async fn stop_background_command(
        &self,
        _c: OperationContext,
        _task: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        Err(unused())
    }
}

impl Search for Ledger {}
impl Checkpoints for Ledger {}
impl Pdf for Ledger {}
impl Git for Ledger {}
impl Fetch for Ledger {}
