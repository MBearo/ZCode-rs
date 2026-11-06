//! `!` bash 模式（docs/specs/rust-cli/tui-input.md §2）：开头输入 `!` 进入、开头按 backspace 退出；发送后命令在执行
//! 环境里执行，界面显示 `! 命令` 与输出，模型收到 `<bash-input>` 与 `<bash-stdout>` 两条消息。

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use crossterm::event::KeyCode;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, OperationContext, Outcome, OutputStream,
    StartCommand, WorkspaceRef,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};
use rust_cli_kernel_api::machine::{Commands, EnvInfo};
use rust_cli_kernel_api::testing::{registry, text_response, unsupported_machine};

use super::support::{GatedModel, Setup, request_text, with_ui};

/// 执行命令的替身（环境事实与命令两个端口）：输出 `ran <命令>`，退出码 0。
#[derive(Default)]
struct ShellEnv {
    commands: Mutex<Vec<(OperationId, StartCommand)>>,
}

#[async_trait]
impl EnvInfo for ShellEnv {
    async fn facts(&self, _workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        Ok(EnvFacts {
            cwd: EnvPath::new("/Users/dev/project"),
            platform: "darwin".into(),
            os_version: "x".into(),
            shell: "zsh".into(),
            is_git_repo: false,
            home: Some(EnvPath::new("/Users/dev")),
            uid: None,
        })
    }
}

#[async_trait]
impl Commands for ShellEnv {
    async fn start_command(
        &self,
        _w: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<BoxStream<'static, Result<CommandEvent, EnvError>>, EnvError> {
        let operation = context.operation.clone();
        self.commands
            .lock()
            .unwrap()
            .push((context.operation, request));
        Ok(self.watch_command(operation, 0))
    }

    fn watch_command(
        &self,
        operation: OperationId,
        _cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        let command = self
            .commands
            .lock()
            .unwrap()
            .iter()
            .find(|(id, _)| *id == operation)
            .map(|(_, start)| start.command.clone())
            .unwrap_or_default();
        let data = format!("ran {command}\n").into_bytes();
        let size = data.len() as u64;
        stream::iter([
            Ok(CommandEvent::Started {
                output: EnvPath::new("/tmp/out"),
            }),
            Ok(CommandEvent::Output {
                stream: OutputStream::Stdout,
                data,
                cursor: size,
            }),
            Ok(CommandEvent::Finished {
                outcome: Outcome::Completed,
                exit_code: Some(0),
                timed_out: false,
                cwd_reset: None,
                output_bytes: size,
                output_limit: None,
            }),
        ])
        .boxed()
    }

    async fn cancel(&self, _operation: OperationId) -> Result<(), EnvError> {
        Ok(())
    }

    async fn close_session(&self, _session: &SessionId, _forget: bool) -> Result<(), EnvError> {
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn bang_runs_a_shell_command_and_the_model_sees_both_messages() {
    let env = Arc::new(ShellEnv::default());
    let setup = Setup {
        environments: registry(
            unsupported_machine()
                .with_info(env.clone())
                .with_commands(env.clone()),
        ),
        ..Setup::default()
    };
    let model = GatedModel::scripted(vec![text_response(&["listed the files"])]);
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("!");
        ui.wait_for("!  Run a shell command").await;
        ui.submit("ls -a");
        let screen = ui
            .wait("the reply", |s| {
                s.contains("listed the files") && !s.contains("esc stop")
            })
            .await;
        assert!(screen.contains("! ls -a"), "{screen}");
        assert!(screen.contains("│ ran ls -a"), "{screen}");
        // 发送后回到普通模式。
        assert!(!screen.contains("Run a shell command"), "{screen}");
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    let text = request_text(&requests[0]);
    assert!(text.contains("<bash-input>ls -a</bash-input>"), "{text}");
    assert!(
        text.contains("<bash-stdout>ran ls -a</bash-stdout><bash-stderr></bash-stderr>"),
        "{text}"
    );
    let commands = env.commands.lock().unwrap();
    assert_eq!(commands[0].1.command, "ls -a");
    assert!(commands[0].1.sandbox.is_none());
}

#[tokio::test(flavor = "multi_thread")]
async fn backspace_at_the_start_leaves_bash_mode() {
    let model = GatedModel::scripted(vec![]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("!");
        ui.wait_for("!  Run a shell command").await;
        ui.key(KeyCode::Backspace);
        ui.wait("prompt mode", |s| !s.contains("Run a shell command"))
            .await;
        // 不在开头的 `!` 只是文字。
        ui.type_text("hi!");
        let screen = ui.wait_for("› hi!").await;
        assert!(!screen.contains("Run a shell command"), "{screen}");
    })
    .await;
}
