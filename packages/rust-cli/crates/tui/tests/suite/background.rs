//! 后台命令（docs/specs/rust-cli/tui-tasks.md §2、§3）：任务行的计数、任务卡的 Commands、展开的输出的读取与
//! 停止。执行环境是只有一条后台命令的替身（账本与输出文件），停止走 agents 的 TaskStop 路径。
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use crossterm::event::{KeyCode, KeyModifiers, MouseButton, MouseEventKind};
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

use super::support::{GatedModel, Setup, row_of, with_ui};

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
async fn the_task_line_counts_the_command_and_the_card_unfolds_its_output() {
    let model = GatedModel::scripted(vec![text_response(&["started the tests"])]);
    let setup = Setup {
        width: 100,
        height: 40,
        environments: registry(Arc::new(OneShell::default()).machine()),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("run the tests in the background");
        // 回合结束后重取后台任务的快照：任务行数到它，右沿是 `^T all tasks`（tui-tasks.md §2）；状态栏里没有 pill。
        let line = ui
            .wait("the task line", |s| {
                s.contains("1 shell running") && !s.contains("esc stop")
            })
            .await;
        assert!(line.contains("^T all tasks"), "{line}");
        // ^T：任务卡落在 Commands（tui-tasks.md §3.1）。
        ui.press(KeyCode::Char('t'), KeyModifiers::CONTROL);
        let card = ui.wait_for("○ npm test").await;
        assert!(card.contains("Commands 1"), "{card}");
        assert!(card.contains("⏎ output"), "{card}");
        // ⏎：行下面展开输出的最后 5 行与说明（用时、行数）。
        ui.key(KeyCode::Enter);
        let peek = ui.wait_for("showing 5 of 12 lines").await;
        assert!(peek.contains("│ test 12 ok"), "{peek}");
        assert!(peek.contains("│ test 8 ok"), "{peek}");
        assert!(!peek.contains("│ test 7 ok"), "{peek}");
        assert!(peek.contains("│ 1m 23s · showing 5 of 12 lines"), "{peek}");
        assert!(peek.contains("⏎ close output"), "{peek}");
        // ^X：与 `/tasks`、TaskStop 同一条停止路径；之后快照里没有它，卡关上，任务行离开。
        ui.press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        ui.wait_for("Stopped npm test").await;
        ui.wait("the line and the card to go away", |s| {
            !s.contains("1 shell running") && !s.contains("○ npm test")
        })
        .await;
    })
    .await;
}

/// 点任务行的 `1 shell running`（tui-tasks.md §2.2「点击」）：按下与松开在同一处，卡打开、落在 Commands。区域是画出来的那一帧
/// 记下的，所以这里验的是整条路：标题行的列、底部栈换成屏幕坐标、松开时的目标。
#[tokio::test(flavor = "multi_thread")]
async fn clicking_the_command_count_opens_the_card_on_commands() {
    let model = GatedModel::scripted(vec![text_response(&["started the tests"])]);
    let setup = Setup {
        width: 100,
        height: 40,
        environments: registry(Arc::new(OneShell::default()).machine()),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("run the tests in the background");
        let screen = ui
            .wait("the task line", |s| {
                s.contains("1 shell running") && !s.contains("esc stop")
            })
            .await;
        let row = row_of(&screen, "1 shell running").unwrap();
        let line = screen.lines().nth(usize::from(row)).unwrap();
        // 前面的 `○` 与 ` · ` 都是一格宽：按字数算列。
        let at = line.find("1 shell running").unwrap();
        let col = u16::try_from(line[..at].chars().count() + 2).unwrap();
        ui.mouse(MouseEventKind::Down(MouseButton::Left), col, row);
        ui.mouse(MouseEventKind::Up(MouseButton::Left), col, row);
        let card = ui.wait_for("○ npm test").await;
        assert!(card.contains("Commands 1"), "{card}");
        ui.key(KeyCode::Esc);
        ui.wait("the card to close", |s| !s.contains("○ npm test"))
            .await;
    })
    .await;
}
