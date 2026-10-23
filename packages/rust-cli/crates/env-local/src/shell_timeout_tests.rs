//! 前台命令到了超时转到后台（docs/specs/rust-cli/shell-permissions.md §1.2.1）：超时后命令接着跑完，输出文件里有
//! 超时前后的输出与结束一行；转后台时登记成后台命令、发一次 `Backgrounded`，结束时记通知；新期限到了按超时结束；
//! 转后台之后的 `cd` 不改会话的当前目录；没有 `on_timeout` 时照旧结束。
#![cfg(unix)]

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{CommandEvent, EnvPath, Outcome, StartCommand, WorkspaceRef};
use rust_cli_kernel_api::env_shell::{BackgroundCommand, CwdScope, TimeoutBackground};
use rust_cli_kernel_api::ids::{EnvironmentId, OperationId, SessionId, ShellTaskId, ToolUseId};

use crate::shell::tests::{Fixture, context, exit_of, fixture, request};

/// 超时 `timeout_ms` 后转后台的前台命令；`deadline_ms` 是转后台之后的期限。
fn timing_out(
    fixture: &Fixture,
    command: &str,
    timeout_ms: u64,
    deadline_ms: Option<u64>,
    scope: CwdScope,
) -> StartCommand {
    let mut start = request(command, &fixture.root, scope);
    start.timeout_ms = Some(timeout_ms);
    start.on_timeout = Some(TimeoutBackground {
        command: BackgroundCommand {
            description: "Long build".to_owned(),
            tool_use: Some(ToolUseId::new("toolu_1")),
        },
        deadline_ms,
    });
    start
}

fn workspace(fixture: &Fixture) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(fixture.root.to_string_lossy()),
    }
}

fn listed(fixture: &Fixture) -> Vec<ShellTaskId> {
    fixture
        .shells
        .shell_state(&workspace(fixture), &SessionId::new("s1"))
        .background
}

/// 收集一条命令的事件：转后台之前的输出、有没有转后台、全部输出与终态。
struct Watched {
    before: Option<String>,
    output: String,
    finished: CommandEvent,
}

async fn watch(fixture: &Fixture, operation: &str, task: &ShellTaskId) -> Watched {
    let mut events = fixture
        .shells
        .watch(&OperationId::new(operation), 0)
        .unwrap();
    let (mut before, mut output) = (None, String::new());
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Started { .. } => {}
            CommandEvent::Output { data, .. } => output.push_str(&String::from_utf8_lossy(&data)),
            CommandEvent::Backgrounded => {
                assert!(before.is_none(), "backgrounded twice");
                // 转后台的那一刻已经登记成后台命令（`-p` 据此等它、TaskStop 能停它）。
                assert!(listed(fixture).contains(task), "{:?}", listed(fixture));
                before = Some(output.clone());
            }
            finished @ CommandEvent::Finished { .. } => {
                return Watched {
                    before,
                    output,
                    finished,
                };
            }
        }
    }
    panic!("no terminal event");
}

#[tokio::test]
async fn a_timed_out_command_keeps_running_in_the_background() {
    let fixture = fixture(Vec::new());
    let start = timing_out(
        &fixture,
        "echo start; sleep 1; echo late",
        300,
        None,
        CwdScope::Pinned,
    );
    let task = start.task.clone();
    fixture.shells.start(context("op1"), start).await.unwrap();
    let watched = watch(&fixture, "op1", &task).await;
    assert_eq!(watched.before.as_deref(), Some("start\n"));
    assert_eq!(
        exit_of(&watched.finished),
        (Outcome::Completed, Some(0), false)
    );
    // 后台命令的结束一行同样写进输出文件。
    assert_eq!(watched.output, "start\nlate\n\n[exited with code 0]\n");
    let notice = fixture
        .shells
        .notices(&SessionId::new("s1"), 0)
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice.task, task);
    assert_eq!(notice.description, "Long build");
    assert_eq!(notice.tool_use, Some(ToolUseId::new("toolu_1")));
    assert_eq!(
        (notice.outcome, notice.exit_code),
        (Outcome::Completed, Some(0))
    );
}

#[tokio::test]
async fn the_background_deadline_ends_a_moved_command() {
    let fixture = fixture(Vec::new());
    let start = timing_out(
        &fixture,
        "echo start; sleep 30",
        200,
        Some(300),
        CwdScope::Pinned,
    );
    let task = start.task.clone();
    let begun = Instant::now();
    fixture.shells.start(context("op1"), start).await.unwrap();
    let watched = watch(&fixture, "op1", &task).await;
    assert!(watched.before.is_some());
    let (outcome, exit, timed_out) = exit_of(&watched.finished);
    assert!(
        matches!(outcome, Outcome::Failed { .. }) && exit == Some(143) && timed_out,
        "{outcome:?} {exit:?} {timed_out}"
    );
    // 期限从转后台时算起：超时 200 ms + 期限 300 ms。
    assert!(begun.elapsed() >= Duration::from_millis(500));
    let notice = fixture
        .shells
        .notices(&SessionId::new("s1"), 0)
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notice.task, task);
    assert!(matches!(notice.outcome, Outcome::Failed { .. }));
}

/// 转后台之后命令里的 `cd` 不改会话的当前目录，也不报重置。
#[tokio::test]
#[expect(clippy::disallowed_methods, reason = "测试准备子目录")]
async fn a_moved_command_does_not_move_the_session_directory() {
    let fixture = fixture(Vec::new());
    std::fs::create_dir_all(fixture.root.join("sub")).unwrap();
    let roots = vec![EnvPath::new(fixture.root.to_string_lossy())];
    let start = timing_out(
        &fixture,
        "sleep 0.6; cd sub",
        200,
        None,
        CwdScope::Session { roots },
    );
    let task = start.task.clone();
    fixture.shells.start(context("op1"), start).await.unwrap();
    let watched = watch(&fixture, "op1", &task).await;
    assert!(watched.before.is_some());
    let CommandEvent::Finished { cwd_reset, .. } = &watched.finished else {
        panic!("not finished");
    };
    assert_eq!(cwd_reset, &None);
    let state = fixture
        .shells
        .shell_state(&workspace(&fixture), &SessionId::new("s1"));
    assert_eq!(state.cwd.as_str(), fixture.root.to_string_lossy());
}

/// 没有 `on_timeout`：照旧按超时结束，不转后台、不登记。
#[tokio::test]
async fn without_on_timeout_a_timed_out_command_is_ended() {
    let fixture = fixture(Vec::new());
    let mut start = request("echo start; sleep 30", &fixture.root, CwdScope::Pinned);
    start.timeout_ms = Some(200);
    let task = start.task.clone();
    fixture.shells.start(context("op1"), start).await.unwrap();
    let watched = watch(&fixture, "op1", &task).await;
    assert!(watched.before.is_none());
    let (outcome, exit, timed_out) = exit_of(&watched.finished);
    assert!(matches!(outcome, Outcome::Failed { .. }) && exit == Some(143) && timed_out);
    assert!(listed(&fixture).is_empty());
}
