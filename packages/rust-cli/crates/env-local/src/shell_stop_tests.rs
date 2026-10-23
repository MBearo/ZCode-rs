//! TaskStop 停后台命令（docs/specs/rust-cli/shell-permissions.md §2.6）：登记只在账本里；停止后不再算
//! 后台工作、不发通知，终态照常经回执给出；已结束未 ack 的报“不在跑”；其余都是“不认识”。
#![cfg(unix)]

use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{EnvError, EnvPath, Outcome, WorkspaceRef};
use rust_cli_kernel_api::env_shell::{
    BackgroundCommand, BackgroundInfo, BackgroundStatus, BackgroundStop, BackgroundTask, CwdScope,
};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId, ShellTaskId};

use crate::shell::tests::{Fixture, collect, context, exit_of, fixture, request};

/// 起一条后台命令，返回它的任务 ID。
async fn background(fixture: &Fixture, operation: &str, command: &str) -> ShellTaskId {
    let mut start = request(command, &fixture.root, CwdScope::Pinned);
    start.background = Some(BackgroundCommand {
        description: "Wait a while".to_owned(),
        tool_use: None,
    });
    let task = start.task.clone();
    fixture
        .shells
        .start(context(operation), start)
        .await
        .unwrap();
    task
}

fn key(session: &str, task: &ShellTaskId) -> BackgroundTask {
    BackgroundTask {
        session: SessionId::new(session),
        task: task.clone(),
    }
}

fn listed(fixture: &Fixture) -> Vec<ShellTaskId> {
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(fixture.root.to_string_lossy()),
    };
    fixture
        .shells
        .shell_state(&workspace, &SessionId::new("s1"))
        .background
}

#[tokio::test]
async fn a_stopped_command_leaves_the_background_without_a_notice() {
    let fixture = fixture(Vec::new());
    let task = background(&fixture, "op1", "sleep 30").await;
    assert_eq!(
        fixture.shells.background_command(&key("s1", &task)),
        Some(BackgroundInfo {
            command: "sleep 30".to_owned(),
            status: BackgroundStatus::Running,
        })
    );
    // 只找本会话的命令。
    assert_eq!(fixture.shells.background_command(&key("s2", &task)), None);
    let stopped = fixture.shells.stop(&context("stop1"), &key("s1", &task));
    assert_eq!(
        stopped,
        Ok(BackgroundStop::Stopped {
            command: "sleep 30".to_owned(),
        })
    );
    // 停止后立即不算后台工作（`-p` 不再等它），再查与再停都是“不认识”（B7）。
    assert!(listed(&fixture).is_empty());
    assert_eq!(fixture.shells.background_command(&key("s1", &task)), None);
    assert_eq!(
        fixture.shells.stop(&context("stop2"), &key("s1", &task)),
        Ok(BackgroundStop::NotFound)
    );
    // 终态照常经回执给出：取消、137；输出文件末尾是 `[killed]`。
    let ran = collect(&fixture, "op1").await;
    assert_eq!(
        exit_of(&ran.finished),
        (Outcome::Cancelled, Some(137), false)
    );
    assert!(ran.output.ends_with("\n[killed]\n"), "{:?}", ran.output);
    // 没有通知：下一条后台命令的通知是 seq 1。
    let next = background(&fixture, "op2", "true").await;
    let notice = fixture
        .shells
        .notices(&SessionId::new("s1"), 0)
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!((notice.seq, notice.task), (1, next));
}

#[tokio::test]
async fn an_ended_command_is_not_running_until_its_notice_is_acked() {
    let fixture = fixture(Vec::new());
    let task = background(&fixture, "op1", "exit 3").await;
    let session = SessionId::new("s1");
    let notice = fixture
        .shells
        .notices(&session, 0)
        .next()
        .await
        .unwrap()
        .unwrap();
    let failed = Some(BackgroundInfo {
        command: "exit 3".to_owned(),
        status: BackgroundStatus::Failed,
    });
    assert_eq!(fixture.shells.background_command(&key("s1", &task)), failed);
    assert_eq!(
        fixture.shells.stop(&context("stop1"), &key("s1", &task)),
        Ok(BackgroundStop::NotRunning {
            status: BackgroundStatus::Failed,
        })
    );
    // 停止失败不影响通知：仍在等 ack。
    assert_eq!(listed(&fixture), std::slice::from_ref(&task));
    fixture.shells.ack(&session, notice.seq);
    assert_eq!(fixture.shells.background_command(&key("s1", &task)), None);
    assert_eq!(
        fixture.shells.stop(&context("stop2"), &key("s1", &task)),
        Ok(BackgroundStop::NotFound)
    );
}

#[tokio::test]
async fn a_stop_replays_by_operation_and_rejects_other_arguments() {
    let fixture = fixture(Vec::new());
    let task = background(&fixture, "op1", "sleep 30").await;
    let first = fixture.shells.stop(&context("stop1"), &key("s1", &task));
    // 同一个 operationId 重放：同样的结果（不是“不认识”）。
    let again = fixture.shells.stop(&context("stop1"), &key("s1", &task));
    assert_eq!(first, again);
    let other = fixture
        .shells
        .stop(&context("stop1"), &key("s1", &ShellTaskId::new("bother")));
    assert!(matches!(other, Err(EnvError::OperationConflict { .. })));
    // 不认识的任务。
    assert_eq!(
        fixture.shells.stop(
            &context("stop3"),
            &key("s1", &ShellTaskId::new("bnotatask"))
        ),
        Ok(BackgroundStop::NotFound)
    );
    let ran = tokio::time::timeout(Duration::from_secs(10), collect(&fixture, "op1"))
        .await
        .unwrap();
    assert_eq!(exit_of(&ran.finished).0, Outcome::Cancelled);
}

/// Shell 详情的 Runtime（tui-panels.md §3.3）：在跑时给出从开始到现在的时长；停止后不再给。
#[tokio::test]
async fn a_running_command_reports_how_long_it_has_run() {
    let fixture = fixture(Vec::new());
    let task = background(&fixture, "op1", "sleep 30").await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let elapsed = fixture.shells.background_elapsed(&key("s1", &task));
    assert!(elapsed.is_some_and(|ms| ms >= 20), "{elapsed:?}");
    assert_eq!(fixture.shells.background_elapsed(&key("s2", &task)), None);
    fixture
        .shells
        .stop(&context("stop1"), &key("s1", &task))
        .unwrap();
    assert_eq!(fixture.shells.background_elapsed(&key("s1", &task)), None);
    collect(&fixture, "op1").await;
}
