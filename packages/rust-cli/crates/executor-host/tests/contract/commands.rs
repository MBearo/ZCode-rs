//! `Commands` 与 `Shell` 的当前目录：输出合流与退出码、超时结束进程树、取消是一条消息、会话 cwd 的延续与重置、
//! 同一个 operationId 只跑一次、环境变量白名单、残留进程回收、hook 的 stdin 与分开的 stderr、会话关闭。
//! 需要 `/bin/bash`，Windows 上跳过。
#![cfg(unix)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{CommandEvent, EnvError, EnvPath, Outcome, OutputStream};
use rust_cli_kernel_api::env_shell::{CwdScope, HookIo};
use rust_cli_kernel_api::ids::{OperationId, SessionId};

use crate::contract;
use crate::shell_support::{exit_of, shell};
use crate::support::{Kind, context};

async fn stdout_and_stderr_share_one_stream_in_order(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let start = shell.request("echo out; echo err >&2; exit 3", CwdScope::Pinned);
    let ran = shell.run("op1", start).await;
    assert_eq!(ran.output, "out\nerr\n");
    assert_eq!(exit_of(&ran.finished), (Outcome::Completed, Some(3), false));
    let CommandEvent::Finished { output_bytes, .. } = ran.finished else {
        unreachable!()
    };
    assert_eq!(output_bytes, 8);
}

async fn timeout_kills_the_tree_with_exit_143(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let mut start = shell.request("sleep 5; echo late", CwdScope::Pinned);
    start.timeout_ms = Some(300);
    let began = Instant::now();
    let ran = shell.run("op1", start).await;
    assert!(began.elapsed() < Duration::from_secs(4));
    assert_eq!(ran.output, "");
    let (outcome, code, timed_out) = exit_of(&ran.finished);
    assert!(matches!(outcome, Outcome::Failed { code: Some(ref c), .. } if c == "timeout"));
    assert_eq!((code, timed_out), (Some(143), true));
}

async fn cancel_is_a_message_and_the_terminal_state_follows(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let commands = shell.machine.commands();
    shell
        .start("op1", shell.request("sleep 5", CwdScope::Pinned))
        .await;
    let op = OperationId::new("op1");
    assert_eq!(
        commands.operation_status(op.clone()).await.unwrap(),
        Outcome::Running
    );
    commands.cancel(op.clone()).await.unwrap();
    let ran = shell.collect("op1").await;
    assert_eq!(
        exit_of(&ran.finished),
        (Outcome::Cancelled, Some(137), false)
    );
    assert_eq!(
        commands.operation_status(op).await.unwrap(),
        Outcome::Cancelled
    );
}

async fn session_cwd_persists_inside_the_roots_and_resets_outside(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let root = EnvPath::new(shell.root.to_string_lossy());
    let scope = CwdScope::Session {
        roots: vec![root.clone()],
    };
    let session = SessionId::new("s1");
    shell
        .run(
            "op1",
            shell.request("mkdir -p sub && cd sub", scope.clone()),
        )
        .await;
    let sub = shell.root.join("sub");
    let state = shell
        .machine
        .shell()
        .shell_state(&shell.workspace(), &session)
        .await
        .unwrap();
    assert_eq!(state.cwd.as_str(), sub.to_string_lossy());
    let ran = shell.run("op2", shell.request("pwd", scope.clone())).await;
    assert_eq!(ran.output.trim_end(), sub.to_string_lossy());
    let ran = shell
        .run("op3", shell.request("cd / && pwd", scope.clone()))
        .await;
    assert_eq!(ran.output, "/\n");
    let CommandEvent::Finished { cwd_reset, .. } = &ran.finished else {
        unreachable!()
    };
    assert_eq!(cwd_reset.as_ref(), Some(&root));
    let ran = shell.run("op4", shell.request("pwd", scope)).await;
    assert_eq!(ran.output.trim_end(), shell.root.to_string_lossy());
    // 会话换了工作目录（EnterWorktree、`/cd`）：下一条命令从那里开始。
    shell
        .machine
        .shell()
        .set_shell_cwd(&session, EnvPath::new(sub.to_string_lossy()))
        .await
        .unwrap();
    let scope = CwdScope::Session { roots: vec![root] };
    let ran = shell.run("op5", shell.request("pwd", scope)).await;
    assert_eq!(ran.output.trim_end(), sub.to_string_lossy());
}

async fn the_same_operation_runs_once_and_other_arguments_are_rejected(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let marker = shell.root.join("count");
    let command = format!("echo x >> '{}'", marker.display());
    let start = shell.request(&command, CwdScope::Pinned);
    shell.run("op1", start.clone()).await;
    shell.start("op1", start).await;
    let other = shell.request("echo other", CwdScope::Pinned);
    let conflict = shell
        .machine
        .commands()
        .start_command(&shell.workspace(), context("op1"), other)
        .await;
    assert!(matches!(conflict, Err(EnvError::OperationConflict { .. })));
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "x\n");
}

async fn only_whitelisted_variables_reach_the_command(kind: Kind) {
    let shell = shell(
        kind,
        vec![
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("ZCODE_API_KEY".to_owned(), "secret".to_owned()),
            ("LC_ALL".to_owned(), "C".to_owned()),
        ],
    )
    .await;
    let start = shell.request(
        "echo \"${ZCODE_API_KEY:-none} $LC_ALL $GIT_EDITOR $ZCODE\"",
        CwdScope::Pinned,
    );
    let ran = shell.run("op1", start).await;
    assert_eq!(ran.output, "none C true 1\n");
}

async fn leftover_processes_are_reaped_when_the_command_exits(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    let ran = shell
        .run("op1", shell.request("sleep 30 & echo $!", CwdScope::Pinned))
        .await;
    let pid = ran.output.trim().to_owned();
    let probe = shell.request(
        &format!("kill -0 {pid} 2>/dev/null; echo $?"),
        CwdScope::Pinned,
    );
    let alive = shell.run("op2", probe).await;
    assert_eq!(alive.output, "1\n");
}

/// (stdout, stderr, 终态, 退出码)
async fn run_hook(
    shell: &crate::shell_support::Shell,
    operation: &str,
    command: &str,
    stdin: String,
) -> (String, String, Outcome, Option<i32>) {
    let mut start = shell.request(command, CwdScope::Pinned);
    start.hook = Some(HookIo {
        stdin,
        env: BTreeMap::from([(
            "ZCODE_PROJECT_DIR".to_owned(),
            "/Users/dev/project".to_owned(),
        )]),
    });
    shell.start(operation, start).await;
    let mut events = shell
        .machine
        .commands()
        .watch_command(OperationId::new(operation), 0);
    let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Started { .. } | CommandEvent::Backgrounded => {}
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
            CommandEvent::Finished {
                outcome, exit_code, ..
            } => {
                return (
                    String::from_utf8(stdout).unwrap(),
                    String::from_utf8(stderr).unwrap(),
                    outcome,
                    exit_code,
                );
            }
        }
    }
    panic!("no terminal event");
}

async fn hook_reads_stdin_and_separates_stderr(kind: Kind) {
    let shell = shell(kind, vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())]).await;
    let (stdout, stderr, outcome, exit) = run_hook(
        &shell,
        "op1",
        r#"input=$(cat); echo "got $input"; echo "dir $ZCODE_PROJECT_DIR"; echo oops >&2; exit 2"#,
        "{\"a\":1}\n".to_owned(),
    )
    .await;
    assert_eq!(stdout, "got {\"a\":1}\ndir /Users/dev/project\n");
    assert_eq!(stderr, "oops\n");
    assert_eq!((outcome, exit), (Outcome::Completed, Some(2)));
}

async fn hook_that_ignores_a_large_stdin_still_finishes(kind: Kind) {
    let shell = shell(kind, vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())]).await;
    let big = "x".repeat(512 * 1024);
    let (stdout, stderr, outcome, exit) = run_hook(&shell, "op2", "echo done", big).await;
    assert_eq!((stdout.as_str(), stderr.as_str()), ("done\n", ""));
    assert_eq!((outcome, exit), (Outcome::Completed, Some(0)));
}

async fn closing_a_session_is_accepted(kind: Kind) {
    let shell = shell(kind, Vec::new()).await;
    shell
        .run("op1", shell.request("true", CwdScope::Pinned))
        .await;
    shell
        .machine
        .commands()
        .close_session(&SessionId::new("s1"), true)
        .await
        .unwrap();
}

contract!(
    stdout_and_stderr_share_one_stream_in_order,
    timeout_kills_the_tree_with_exit_143,
    cancel_is_a_message_and_the_terminal_state_follows,
    session_cwd_persists_inside_the_roots_and_resets_outside,
    the_same_operation_runs_once_and_other_arguments_are_rejected,
    only_whitelisted_variables_reach_the_command,
    leftover_processes_are_reaped_when_the_command_exits,
    hook_reads_stdin_and_separates_stderr,
    hook_that_ignores_a_large_stdin_still_finishes,
    closing_a_session_is_accepted,
);
