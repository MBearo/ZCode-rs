//! 后台任务（docs/specs/rust-cli/tui-panels.md §3）：footer 的 pill 选中、Background 面板、Shell 详情的读取与
//! 停止。执行环境是只有一条后台命令的替身（账本与输出文件），停止走 agents 的 TaskStop 路径。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{
    EnvError, EnvPath, FileContent, FileVersion, OperationContext, ReadFile, WorkspaceRef,
    WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::{ReadText, TextRead, TextSlice};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundStatus, BackgroundStop, BackgroundTask, ShellState,
};
use rust_cli_kernel_api::ids::{SessionId, ShellTaskId};
use rust_cli_kernel_api::machine::{Background, Files, MachineEnv, Shell};
use rust_cli_kernel_api::testing::{registry, text_response, unsupported_machine};

use super::support::{GatedModel, Setup, with_ui};

fn unused() -> EnvError {
    EnvError::Unsupported {
        what: "test".to_owned(),
    }
}

/// 一条在跑的后台命令 `b1`（`npm test`），输出 12 行；停止后账本里就没有它了。
#[derive(Default)]
struct OneShell {
    stopped: AtomicBool,
}

impl OneShell {
    /// 机器只换上文件、shell 与后台命令三个端口（其余端口返回 `Unsupported`）。
    fn machine(self: Arc<Self>) -> MachineEnv {
        unsupported_machine()
            .with_files(self.clone())
            .with_shell(self.clone())
            .with_background(self)
    }
}

#[async_trait]
impl Files for OneShell {
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

    async fn read_text(&self, _w: &WorkspaceRef, request: ReadText) -> Result<TextRead, EnvError> {
        assert_eq!(request.path.as_str(), "/Users/dev/tasks/b1.output");
        let content = (1..=12)
            .map(|n| format!("test {n} ok"))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(TextRead::Text(TextSlice {
            content,
            line_count: 12,
            total_lines: 12,
            total_bytes: 0,
            read_bytes: 0,
            mtime_ms: 0,
            version: FileVersion::new("v1"),
        }))
    }
}

#[async_trait]
impl Shell for OneShell {
    async fn shell_state(&self, _w: &WorkspaceRef, _s: &SessionId) -> Result<ShellState, EnvError> {
        let background = if self.stopped.load(Ordering::SeqCst) {
            Vec::new()
        } else {
            vec![ShellTaskId::new("b1")]
        };
        Ok(ShellState {
            cwd: EnvPath::new("/Users/dev/project"),
            outputs: Some(EnvPath::new("/Users/dev/tasks")),
            background,
        })
    }
}

#[async_trait]
impl Background for OneShell {
    async fn background_command(
        &self,
        task: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        let known = task.task.as_str() == "b1" && !self.stopped.load(Ordering::SeqCst);
        Ok(known.then(|| BackgroundInfo {
            command: "npm test".to_owned(),
            status: BackgroundStatus::Running,
            ..BackgroundInfo::default()
        }))
    }

    async fn background_elapsed(&self, _task: BackgroundTask) -> Result<Option<u64>, EnvError> {
        Ok((!self.stopped.load(Ordering::SeqCst)).then_some(83_000))
    }

    async fn stop_background_command(
        &self,
        _c: OperationContext,
        _task: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        self.stopped.store(true, Ordering::SeqCst);
        Ok(BackgroundStop::Stopped {
            command: "npm test".to_owned(),
        })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pill_opens_the_panel_and_the_shell_details() {
    let model = GatedModel::scripted(vec![text_response(&["started the tests"])]);
    let setup = Setup {
        // 详情约 23 行；底部区域最高是屏幕的 2/3。
        width: 100,
        height: 40,
        environments: registry(Arc::new(OneShell::default()).machine()),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("run the tests in the background");
        // 回合结束后重取后台任务的快照：footer 有 `1 shell`。
        let screen = ui
            .wait("the pill", |s| {
                s.contains("· 1 shell") && !s.contains("esc to interrupt")
            })
            .await;
        assert!(screen.contains("↓ to manage"), "{screen}");
        ui.key(KeyCode::Down);
        ui.wait_for("· 1 shell · Enter to view tasks").await;
        ui.key(KeyCode::Enter);
        let panel = ui.wait_for("1 active shell").await;
        assert!(panel.contains("  Background"), "{panel}");
        assert!(
            panel
                .lines()
                .any(|l| l.contains("npm test") && l.ends_with("running")),
            "{panel}"
        );
        // enter：Shell 详情（状态、用时、命令、输出的最后 10 行）。
        ui.key(KeyCode::Enter);
        let detail = ui.wait_for("Showing 10 of 12 lines").await;
        assert!(detail.contains("  Shell details"), "{detail}");
        assert!(detail.contains("Status: running"), "{detail}");
        assert!(detail.contains("Runtime: 1m 23s"), "{detail}");
        assert!(detail.contains("Command: npm test"), "{detail}");
        assert!(detail.contains("│ test 3 ok"), "{detail}");
        assert!(!detail.contains("│ test 2 ok"), "{detail}");
        assert!(
            detail.contains("← to go back · x to stop · Esc to close"),
            "{detail}"
        );
        // x：与 `/tasks`、TaskStop 同一条停止路径；之后快照里没有它，footer 的 pill 消失。
        ui.key(KeyCode::Char('x'));
        ui.wait_for("Stopped npm test").await;
        ui.key(KeyCode::Esc);
        ui.wait("the pill to go away", |s| {
            !s.contains("1 shell") && !s.contains("Shell details")
        })
        .await;
    })
    .await;
}
