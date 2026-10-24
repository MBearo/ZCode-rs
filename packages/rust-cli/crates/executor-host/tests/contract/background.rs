//! `Background`、`Shell` 的通知与超时转后台：后台命令的通知在 ack 之前反复送达、ack 之后不再算后台工作；TaskStop 停的
//! 命令不发通知、终态照常；已结束未 ack 的“不在跑”；停止按 operationId 重放；运行时长；超时转后台与转后台之后的期限；
//! Monitor 的事件只有 stdout。需要 `/bin/bash`，Windows 上跳过。
#![cfg(unix)]

use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvPath, Outcome, OutputStream, StartCommand,
};
use rust_cli_kernel_api::env_shell::{
    BackgroundCommand, BackgroundInfo, BackgroundKind, BackgroundStatus, BackgroundStop,
    BackgroundTask, CwdScope, ShellNotice, TimeoutBackground,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId, ShellTaskId, ToolUseId};

use crate::contract;
use crate::shell_support::{Shell, exit_of, shell};
use crate::support::{Kind, context};

fn key(session: &str, task: &ShellTaskId) -> BackgroundTask {
    BackgroundTask {
        session: SessionId::new(session),
        task: task.clone(),
    }
}

/// 起一条后台命令，返回它的任务 ID。
async fn background(shell: &Shell, operation: &str, command: &str) -> ShellTaskId {
    let mut start = shell.request(command, CwdScope::Pinned);
    start.background = Some(BackgroundCommand {
        description: "Wait a while".to_owned(),
        tool_use: None,
        kind: Default::default(),
    });
    let task = start.task.clone();
    shell.start(operation, start).await;
    task
}

async fn next_notice(shell: &Shell) -> ShellNotice {
    let mut notices = shell
        .machine
        .shell()
        .shell_notices(&SessionId::new("s1"), 0);
    tokio::time::timeout(Duration::from_secs(10), notices.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap()
}

async fn info(shell: &Shell, session: &str, task: &ShellTaskId) -> Option<BackgroundInfo> {
    shell
        .machine
        .background()
        .background_command(key(session, task))
        .await
        .unwrap()
}

async fn stop(
    shell: &Shell,
    operation: &str,
    task: &ShellTaskId,
) -> Result<BackgroundStop, EnvError> {
    shell
        .machine
        .background()
        .stop_background_command(context(operation), key("s1", task))
        .await
}

async fn background_commands_report_until_acked(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let mut start = shell.request("echo bg", CwdScope::Pinned);
    start.background = Some(BackgroundCommand {
        description: "Echo".to_owned(),
        tool_use: None,
        kind: Default::default(),
    });
    let task = start.task.clone();
    shell.start("op1", start).await;
    // 启动后就在后台列表里（不论是否已经跑完）。
    assert_eq!(shell.background().await, std::slice::from_ref(&task));
    let notice = next_notice(&shell).await;
    assert_eq!((notice.seq, notice.exit_code), (1, Some(0)));
    assert_eq!(notice.description, "Echo");
    let written = std::fs::read_to_string(notice.output.as_str()).unwrap();
    assert_eq!(written, "bg\n\n[exited with code 0]\n");
    // 没有 ack：重新订阅仍会送达；已结束但没有 ack，仍算后台工作。
    assert_eq!(next_notice(&shell).await.seq, 1);
    assert_eq!(shell.background().await, [task]);
    let session = SessionId::new("s1");
    shell
        .machine
        .shell()
        .ack_shell_notice(&session, 1)
        .await
        .unwrap();
    assert!(shell.background().await.is_empty());
    let mut notices = shell.machine.shell().shell_notices(&session, 0);
    let pending = tokio::time::timeout(Duration::from_millis(100), notices.next()).await;
    assert!(pending.is_err(), "acked notices are not delivered again");
}

async fn a_stopped_command_leaves_the_background_without_a_notice(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let task = background(&shell, "op1", "sleep 30").await;
    let running = BackgroundInfo {
        command: "sleep 30".to_owned(),
        status: BackgroundStatus::Running,
    };
    assert_eq!(info(&shell, "s1", &task).await, Some(running));
    // 只找本会话的命令。
    assert_eq!(info(&shell, "s2", &task).await, None);
    assert_eq!(
        stop(&shell, "stop1", &task).await,
        Ok(BackgroundStop::Stopped {
            command: "sleep 30".to_owned(),
        })
    );
    assert!(shell.background().await.is_empty());
    assert_eq!(info(&shell, "s1", &task).await, None);
    assert_eq!(
        stop(&shell, "stop2", &task).await,
        Ok(BackgroundStop::NotFound)
    );
    let ran = shell.collect("op1").await;
    assert_eq!(
        exit_of(&ran.finished),
        (Outcome::Cancelled, Some(137), false)
    );
    assert!(ran.output.ends_with("\n[killed]\n"), "{:?}", ran.output);
    // 没有通知：下一条后台命令的通知是 seq 1。
    let next = background(&shell, "op2", "true").await;
    let notice = next_notice(&shell).await;
    assert_eq!((notice.seq, notice.task), (1, next));
}

async fn an_ended_command_is_not_running_until_its_notice_is_acked(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let task = background(&shell, "op1", "exit 3").await;
    let notice = next_notice(&shell).await;
    let failed = Some(BackgroundInfo {
        command: "exit 3".to_owned(),
        status: BackgroundStatus::Failed,
    });
    assert_eq!(info(&shell, "s1", &task).await, failed);
    assert_eq!(
        stop(&shell, "stop1", &task).await,
        Ok(BackgroundStop::NotRunning {
            status: BackgroundStatus::Failed,
        })
    );
    assert_eq!(shell.background().await, std::slice::from_ref(&task));
    shell
        .machine
        .shell()
        .ack_shell_notice(&SessionId::new("s1"), notice.seq)
        .await
        .unwrap();
    assert_eq!(info(&shell, "s1", &task).await, None);
    assert_eq!(
        stop(&shell, "stop2", &task).await,
        Ok(BackgroundStop::NotFound)
    );
}

async fn a_stop_replays_by_operation_and_rejects_other_arguments(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let task = background(&shell, "op1", "sleep 30").await;
    let first = stop(&shell, "stop1", &task).await;
    assert_eq!(first, stop(&shell, "stop1", &task).await);
    let other = stop(&shell, "stop1", &ShellTaskId::new("bother")).await;
    assert!(matches!(other, Err(EnvError::OperationConflict { .. })));
    assert_eq!(
        stop(&shell, "stop3", &ShellTaskId::new("bnotatask")).await,
        Ok(BackgroundStop::NotFound)
    );
    let ran = tokio::time::timeout(Duration::from_secs(10), shell.collect("op1"))
        .await
        .unwrap();
    assert_eq!(exit_of(&ran.finished).0, Outcome::Cancelled);
}

async fn a_running_command_reports_how_long_it_has_run(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let task = background(&shell, "op1", "sleep 30").await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    let background = shell.machine.background();
    let elapsed = background
        .background_elapsed(key("s1", &task))
        .await
        .unwrap();
    assert!(elapsed.is_some_and(|ms| ms >= 20), "{elapsed:?}");
    assert_eq!(
        background
            .background_elapsed(key("s2", &task))
            .await
            .unwrap(),
        None
    );
    stop(&shell, "stop1", &task).await.unwrap();
    assert_eq!(
        background
            .background_elapsed(key("s1", &task))
            .await
            .unwrap(),
        None
    );
    shell.collect("op1").await;
}

fn timing_out(
    shell: &Shell,
    command: &str,
    timeout_ms: u64,
    deadline_ms: Option<u64>,
) -> StartCommand {
    let mut start = shell.request(command, CwdScope::Pinned);
    start.timeout_ms = Some(timeout_ms);
    start.on_timeout = Some(TimeoutBackground {
        command: BackgroundCommand {
            description: "Long build".to_owned(),
            tool_use: Some(ToolUseId::new("toolu_1")),
            kind: Default::default(),
        },
        deadline_ms,
    });
    start
}

/// 转后台之前的输出、全部输出与终态。
async fn watch_moved(
    shell: &Shell,
    operation: &str,
    task: &ShellTaskId,
) -> (Option<String>, String, CommandEvent) {
    let mut events = shell
        .machine
        .commands()
        .watch_command(OperationId::new(operation), 0);
    let (mut before, mut output) = (None, String::new());
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Started { .. } => {}
            CommandEvent::Output { data, .. } => output.push_str(&String::from_utf8_lossy(&data)),
            CommandEvent::Backgrounded => {
                assert!(before.is_none(), "backgrounded twice");
                assert!(shell.background().await.contains(task));
                before = Some(output.clone());
            }
            finished @ CommandEvent::Finished { .. } => return (before, output, finished),
        }
    }
    panic!("no terminal event");
}

async fn a_timed_out_command_keeps_running_in_the_background(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let start = timing_out(&shell, "echo start; sleep 1; echo late", 300, None);
    let task = start.task.clone();
    shell.start("op1", start).await;
    let (before, output, finished) = watch_moved(&shell, "op1", &task).await;
    assert_eq!(before.as_deref(), Some("start\n"));
    assert_eq!(exit_of(&finished), (Outcome::Completed, Some(0), false));
    assert_eq!(output, "start\nlate\n\n[exited with code 0]\n");
    let notice = next_notice(&shell).await;
    assert_eq!(notice.task, task);
    assert_eq!(notice.description, "Long build");
    assert_eq!(notice.tool_use, Some(ToolUseId::new("toolu_1")));
    assert_eq!(
        (notice.outcome, notice.exit_code),
        (Outcome::Completed, Some(0))
    );
}

async fn the_background_deadline_ends_a_moved_command(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let start = timing_out(&shell, "echo start; sleep 30", 200, Some(300));
    let task = start.task.clone();
    let begun = Instant::now();
    shell.start("op1", start).await;
    let (before, _, finished) = watch_moved(&shell, "op1", &task).await;
    assert!(before.is_some());
    let (outcome, exit, timed_out) = exit_of(&finished);
    assert!(matches!(outcome, Outcome::Failed { .. }) && exit == Some(143) && timed_out);
    assert!(begun.elapsed() >= Duration::from_millis(500));
    let notice = next_notice(&shell).await;
    assert_eq!(notice.task, task);
    assert!(matches!(notice.outcome, Outcome::Failed { .. }));
}

async fn a_moved_command_does_not_move_the_session_directory(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    std::fs::create_dir_all(shell.root.join("sub")).unwrap();
    let roots = vec![EnvPath::new(shell.root.to_string_lossy())];
    let mut start = timing_out(&shell, "sleep 0.6; cd sub", 200, None);
    start.cwd_scope = CwdScope::Session { roots };
    let task = start.task.clone();
    shell.start("op1", start).await;
    let (before, _, finished) = watch_moved(&shell, "op1", &task).await;
    assert!(before.is_some());
    let CommandEvent::Finished { cwd_reset, .. } = &finished else {
        panic!("not finished");
    };
    assert_eq!(cwd_reset, &None);
    let state = shell
        .machine
        .shell()
        .shell_state(&shell.workspace(), &SessionId::new("s1"))
        .await
        .unwrap();
    assert_eq!(state.cwd.as_str(), shell.root.to_string_lossy());
}

async fn without_on_timeout_a_timed_out_command_is_ended(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let mut start = shell.request("echo start; sleep 30", CwdScope::Pinned);
    start.timeout_ms = Some(200);
    let task = start.task.clone();
    shell.start("op1", start).await;
    let (before, _, finished) = watch_moved(&shell, "op1", &task).await;
    assert!(before.is_none());
    let (outcome, exit, timed_out) = exit_of(&finished);
    assert!(matches!(outcome, Outcome::Failed { .. }) && exit == Some(143) && timed_out);
    assert!(shell.background().await.is_empty());
}

async fn a_monitor_streams_only_stdout_and_reports_its_kind(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let mut start = shell.request("echo one; echo oops >&2; echo two", CwdScope::Pinned);
    start.timeout_ms = None;
    start.background = Some(BackgroundCommand {
        description: "watch".to_owned(),
        tool_use: None,
        kind: BackgroundKind::Monitor,
    });
    shell.start("op_m", start).await;
    let mut events = shell
        .machine
        .commands()
        .watch_command(OperationId::new("op_m"), 0);
    let (mut stdout, mut stderr, mut finished) = (Vec::new(), Vec::new(), None);
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Output {
                stream: OutputStream::Stdout,
                data,
                ..
            } => stdout.extend(data),
            CommandEvent::Output {
                stream: OutputStream::Stderr,
                data,
                ..
            } => stderr.extend(data),
            done @ CommandEvent::Finished { .. } => finished = Some(done),
            _ => {}
        }
    }
    assert_eq!(String::from_utf8(stdout).unwrap(), "one\ntwo\n");
    assert_eq!(String::from_utf8(stderr).unwrap(), "");
    assert!(matches!(
        finished,
        Some(CommandEvent::Finished {
            exit_code: Some(0),
            ..
        })
    ));
    let notice = next_notice(&shell).await;
    assert_eq!(notice.kind, BackgroundKind::Monitor);
    assert_eq!(notice.output_bytes, 8);
    let written = std::fs::read_to_string(notice.output.as_str()).unwrap();
    let mut chunks: Vec<&str> = written.split_inclusive('\n').collect();
    chunks.sort_unstable();
    assert_eq!(chunks, ["[stderr] oops\n", "one\n", "two\n"], "{written}");
}

contract!(
    background_commands_report_until_acked,
    a_stopped_command_leaves_the_background_without_a_notice,
    an_ended_command_is_not_running_until_its_notice_is_acked,
    a_stop_replays_by_operation_and_rejects_other_arguments,
    a_running_command_reports_how_long_it_has_run,
    a_timed_out_command_keeps_running_in_the_background,
    the_background_deadline_ends_a_moved_command,
    a_moved_command_does_not_move_the_session_directory,
    without_on_timeout_a_timed_out_command_is_ended,
    a_monitor_streams_only_stdout_and_reports_its_kind,
);
