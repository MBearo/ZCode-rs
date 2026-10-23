//! hook 命令（`StartCommand::hook`）：stdin 输入、stderr 分流、额外环境变量（docs/specs/rust-cli/hooks.md §5.1）。
//! 需要 `/bin/bash`，Windows 上跳过。
#![cfg(unix)]

use std::collections::BTreeMap;
use std::path::PathBuf;

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvPath, OperationContext, Origin, Outcome, OutputStream, StartCommand,
};
use rust_cli_kernel_api::env_shell::{CwdScope, HookIo};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, SessionId, ShellTaskId,
};

use super::*;

#[expect(clippy::disallowed_methods, reason = "测试准备工作区目录")]
fn shells() -> (tempfile::TempDir, PathBuf, Shells) {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let root = base.join("work");
    std::fs::create_dir_all(&root).unwrap();
    let shells = Shells::new(ShellConfig {
        scratch: base.join("scratch"),
        ledger: base.join("ledger"),
        bash: Some(PathBuf::from("/bin/bash")),
        bwrap: None,
        embedded: None,
        env: vec![("PATH".to_owned(), "/usr/bin:/bin".to_owned())].into(),
    });
    (dir, root, shells)
}

fn hook_request(command: &str, root: &std::path::Path, task: &str, stdin: String) -> StartCommand {
    StartCommand {
        command: command.to_owned(),
        cwd: EnvPath::new(root.to_string_lossy()),
        timeout_ms: Some(10_000),
        task: ShellTaskId::new(task),
        shell: Default::default(),
        cwd_scope: CwdScope::Pinned,
        background: None,
        sandbox: None,
        hook: Some(HookIo {
            stdin,
            env: BTreeMap::from([(
                "ZCODE_PROJECT_DIR".to_owned(),
                "/Users/dev/project".to_owned(),
            )]),
        }),
        on_timeout: None,
    }
}

fn context(operation: &str) -> OperationContext {
    OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: OperationId::new(operation),
        origin: Origin::User {
            session: Some(SessionId::new("s1")),
        },
    }
}

/// (stdout, stderr, 终态, 退出码)
async fn run(
    shells: &Shells,
    operation: &str,
    start: StartCommand,
) -> (String, String, Outcome, Option<i32>) {
    shells.start(context(operation), start).await.unwrap();
    let mut events = shells.watch(&OperationId::new(operation), 0).unwrap();
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

#[tokio::test]
async fn hook_reads_stdin_and_separates_stderr() {
    let (_dir, root, shells) = shells();
    let start = hook_request(
        r#"input=$(cat); echo "got $input"; echo "dir $ZCODE_PROJECT_DIR"; echo oops >&2; exit 2"#,
        &root,
        "h1",
        "{\"a\":1}\n".to_owned(),
    );
    let (stdout, stderr, outcome, exit) = run(&shells, "op1", start).await;
    assert_eq!(stdout, "got {\"a\":1}\ndir /Users/dev/project\n");
    assert_eq!(stderr, "oops\n");
    assert_eq!((outcome, exit), (Outcome::Completed, Some(2)));
}

#[tokio::test]
async fn hook_that_ignores_a_large_stdin_still_finishes() {
    let (_dir, root, shells) = shells();
    let big = "x".repeat(512 * 1024);
    let start = hook_request("echo done", &root, "h2", big);
    let (stdout, stderr, outcome, exit) = run(&shells, "op2", start).await;
    assert_eq!((stdout.as_str(), stderr.as_str()), ("done\n", ""));
    assert_eq!((outcome, exit), (Outcome::Completed, Some(0)));
}
