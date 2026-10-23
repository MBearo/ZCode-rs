//! 本机 shell：输出合流、退出码、超时、取消、cwd 延续与重置、幂等、后台通知、环境投影、进程树回收。
//! 需要 `/bin/bash`，Windows 上跳过（那里的 Bash 工具要 Git Bash）。
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvPath, OperationContext, Origin, Outcome, StartCommand, WorkspaceRef,
};
use rust_cli_kernel_api::env_shell::{BackgroundCommand, CwdScope};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId, ShellTaskId,
};

use super::*;

static NEXT_TASK: AtomicU64 = AtomicU64::new(1);

pub(super) struct Fixture {
    _dir: tempfile::TempDir,
    pub(super) root: PathBuf,
    pub(super) shells: Shells,
}

#[expect(clippy::disallowed_methods, reason = "测试准备工作区目录")]
pub(super) fn fixture(env: Vec<(String, String)>) -> Fixture {
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
        env: env.into(),
    });
    Fixture {
        _dir: dir,
        root,
        shells,
    }
}

pub(super) fn context(operation: &str) -> OperationContext {
    OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: OperationId::new(operation),
        origin: Origin::Agent {
            session: SessionId::new("s1"),
            run: RunId::new("r1"),
        },
    }
}

pub(super) fn request(command: &str, root: &Path, scope: CwdScope) -> StartCommand {
    StartCommand {
        command: command.to_owned(),
        cwd: EnvPath::new(root.to_string_lossy()),
        timeout_ms: Some(10_000),
        // 真实调用里内核从 operationId 派生任务 ID，每次调用不同；这里用计数器。
        task: ShellTaskId::new(format!("b{}", NEXT_TASK.fetch_add(1, Ordering::Relaxed))),
        shell: Default::default(),
        cwd_scope: scope,
        background: None,
        sandbox: None,
        hook: None,
        on_timeout: None,
    }
}

pub(super) struct Ran {
    pub(super) output: String,
    pub(super) finished: CommandEvent,
}

async fn run(fixture: &Fixture, operation: &str, start: StartCommand) -> Ran {
    fixture
        .shells
        .start(context(operation), start)
        .await
        .unwrap();
    collect(fixture, operation).await
}

pub(super) async fn collect(fixture: &Fixture, operation: &str) -> Ran {
    let mut events = fixture
        .shells
        .watch(&OperationId::new(operation), 0)
        .unwrap();
    assert!(matches!(
        events.next().await,
        Some(Ok(CommandEvent::Started { .. }))
    ));
    let mut output = Vec::new();
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Output { data, .. } => output.extend(data),
            finished @ CommandEvent::Finished { .. } => {
                return Ran {
                    output: String::from_utf8(output).unwrap(),
                    finished,
                };
            }
            CommandEvent::Started { .. } => panic!("started twice"),
            CommandEvent::Backgrounded => panic!("moved to the background"),
        }
    }
    panic!("no terminal event");
}

pub(super) fn exit_of(event: &CommandEvent) -> (Outcome, Option<i32>, bool) {
    let CommandEvent::Finished {
        outcome,
        exit_code,
        timed_out,
        ..
    } = event
    else {
        panic!("not finished");
    };
    (outcome.clone(), *exit_code, *timed_out)
}

#[tokio::test]
async fn stdout_and_stderr_share_one_stream_in_order() {
    let fixture = fixture(Vec::new());
    let start = request(
        "echo out; echo err >&2; exit 3",
        &fixture.root,
        CwdScope::Pinned,
    );
    let ran = run(&fixture, "op1", start).await;
    assert_eq!(ran.output, "out\nerr\n");
    assert_eq!(exit_of(&ran.finished), (Outcome::Completed, Some(3), false));
    let CommandEvent::Finished { output_bytes, .. } = ran.finished else {
        unreachable!()
    };
    assert_eq!(output_bytes, 8);
}

#[tokio::test]
async fn timeout_kills_the_tree_with_exit_143() {
    let fixture = fixture(Vec::new());
    let mut start = request("sleep 5; echo late", &fixture.root, CwdScope::Pinned);
    start.timeout_ms = Some(300);
    let began = Instant::now();
    let ran = run(&fixture, "op1", start).await;
    assert!(began.elapsed() < Duration::from_secs(4));
    assert_eq!(ran.output, "");
    let (outcome, code, timed_out) = exit_of(&ran.finished);
    assert!(matches!(outcome, Outcome::Failed { code: Some(ref c), .. } if c == "timeout"));
    assert_eq!((code, timed_out), (Some(143), true));
}

#[tokio::test]
async fn cancel_is_a_message_and_the_terminal_state_follows() {
    let fixture = fixture(Vec::new());
    let start = request("sleep 5", &fixture.root, CwdScope::Pinned);
    fixture.shells.start(context("op1"), start).await.unwrap();
    assert_eq!(
        fixture.shells.status(&OperationId::new("op1")),
        Some(Outcome::Running)
    );
    assert!(fixture.shells.cancel(&OperationId::new("op1")));
    let ran = collect(&fixture, "op1").await;
    assert_eq!(
        exit_of(&ran.finished),
        (Outcome::Cancelled, Some(137), false)
    );
    assert_eq!(
        fixture.shells.status(&OperationId::new("op1")),
        Some(Outcome::Cancelled)
    );
}

#[tokio::test]
async fn session_cwd_persists_inside_the_roots_and_resets_outside() {
    let fixture = fixture(Vec::new());
    let root = EnvPath::new(fixture.root.to_string_lossy());
    let scope = CwdScope::Session {
        roots: vec![root.clone()],
    };
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: root.clone(),
    };
    let session = SessionId::new("s1");
    run(
        &fixture,
        "op1",
        request("mkdir -p sub && cd sub", &fixture.root, scope.clone()),
    )
    .await;
    let sub = fixture.root.join("sub");
    assert_eq!(
        fixture
            .shells
            .shell_state(&workspace, &session)
            .cwd
            .as_str(),
        sub.to_string_lossy()
    );
    let ran = run(
        &fixture,
        "op2",
        request("pwd", &fixture.root, scope.clone()),
    )
    .await;
    assert_eq!(ran.output.trim_end(), sub.to_string_lossy());
    let ran = run(
        &fixture,
        "op3",
        request("cd / && pwd", &fixture.root, scope.clone()),
    )
    .await;
    assert_eq!(ran.output, "/\n");
    let CommandEvent::Finished { cwd_reset, .. } = &ran.finished else {
        unreachable!()
    };
    assert_eq!(cwd_reset.as_ref(), Some(&root));
    let ran = run(&fixture, "op4", request("pwd", &fixture.root, scope)).await;
    assert_eq!(ran.output.trim_end(), fixture.root.to_string_lossy());
}

#[tokio::test]
async fn the_same_operation_runs_once_and_other_arguments_are_rejected() {
    let fixture = fixture(Vec::new());
    let marker = fixture.root.join("count");
    let command = format!("echo x >> '{}'", marker.display());
    let start = request(&command, &fixture.root, CwdScope::Pinned);
    run(&fixture, "op1", start.clone()).await;
    fixture.shells.start(context("op1"), start).await.unwrap();
    let other = request("echo other", &fixture.root, CwdScope::Pinned);
    let conflict = fixture.shells.start(context("op1"), other).await;
    assert!(matches!(conflict, Err(EnvError::OperationConflict { .. })));
    #[expect(clippy::disallowed_methods, reason = "测试读回标记文件")]
    let written = std::fs::read_to_string(&marker).unwrap();
    assert_eq!(written, "x\n");
}

#[tokio::test]
async fn background_commands_report_once_acked() {
    let fixture = fixture(Vec::new());
    let mut start = request("echo bg", &fixture.root, CwdScope::Pinned);
    start.background = Some(BackgroundCommand {
        description: "Echo".to_owned(),
        tool_use: None,
    });
    let task = start.task.clone();
    fixture.shells.start(context("op1"), start).await.unwrap();
    let session = SessionId::new("s1");
    let workspace = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(fixture.root.to_string_lossy()),
    };
    let background = || fixture.shells.shell_state(&workspace, &session).background;
    // 启动后就在后台列表里（不论是否已经跑完）。
    assert_eq!(background(), std::slice::from_ref(&task));
    let notice = fixture
        .shells
        .notices(&session, 0)
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!((notice.seq, notice.exit_code), (1, Some(0)));
    assert_eq!(notice.description, "Echo");
    // 输出文件末尾记下终态。
    #[expect(clippy::disallowed_methods, reason = "测试读回后台命令的输出文件")]
    let written = std::fs::read_to_string(notice.output.as_str()).unwrap();
    assert_eq!(written, "bg\n\n[exited with code 0]\n");
    // 没有 ack：重新订阅仍会送达。
    let again = fixture
        .shells
        .notices(&session, 0)
        .next()
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.seq, 1);
    // 已结束但没有 ack：仍算后台工作。
    assert_eq!(background(), [task]);
    fixture.shells.ack(&session, 1);
    assert!(background().is_empty());
    let pending = tokio::time::timeout(
        Duration::from_millis(100),
        fixture.shells.notices(&session, 0).next(),
    )
    .await;
    assert!(pending.is_err(), "acked notices are not delivered again");
}

#[tokio::test]
async fn only_whitelisted_variables_reach_the_command() {
    let fixture = fixture(vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("ZCODE_API_KEY".to_owned(), "secret".to_owned()),
        ("LC_ALL".to_owned(), "C".to_owned()),
    ]);
    let start = request(
        "echo \"${ZCODE_API_KEY:-none} $LC_ALL $GIT_EDITOR $ZCODE\"",
        &fixture.root,
        CwdScope::Pinned,
    );
    let ran = run(&fixture, "op1", start).await;
    assert_eq!(ran.output, "none C true 1\n");
}

#[tokio::test]
async fn leftover_processes_are_reaped_when_the_command_exits() {
    let fixture = fixture(Vec::new());
    let start = request("sleep 30 & echo $!", &fixture.root, CwdScope::Pinned);
    let ran = run(&fixture, "op1", start).await;
    let pid = ran.output.trim().to_owned();
    let probe = request(
        &format!("kill -0 {pid} 2>/dev/null; echo $?"),
        &fixture.root,
        CwdScope::Pinned,
    );
    let alive = run(&fixture, "op2", probe).await;
    assert_eq!(alive.output, "1\n");
}

#[test]
fn script_quotes_the_command_and_records_the_cwd() {
    // 名字里没有 bash / zsh 的 shell 不加关掉扩展 glob 的一段（F44，见 shell_choice_tests.rs）。
    let text = supervise::script(
        "echo 'a'",
        Some(Path::new("/t/b1.cwd")),
        false,
        Path::new("/bin/sh"),
        None,
    );
    assert_eq!(
        text,
        r"eval 'echo '\''a'\''' < /dev/null && pwd -P >| '/t/b1.cwd'"
    );
}

/// 缺陷 R2-1：输出文件与进程账本原来在共享的 `<tmp>/zcode-shell`，默认权限，别的用户能读输出、
/// 抢先建目录。现在都在本用户私有的临时目录里：目录 0700，输出文件 0600。
#[tokio::test]
#[expect(clippy::disallowed_methods, reason = "测试检查目录与文件的权限")]
async fn outputs_live_in_a_private_directory() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = fixture(Vec::new());
    let start = request("echo hi", &fixture.root, CwdScope::Pinned);
    let output = format!("{}.output", start.task);
    run(&fixture, "op1", start).await;
    let private = fixture.root.parent().unwrap().join("scratch");
    let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&private), 0o700);
    assert_eq!(mode(&private.join("s1/tasks").join(output)), 0o600);
}

/// 缺陷 R2-13：`ShellConfig` 是 `derive(Debug)`，原来 `{:?}` 会打出整份进程环境的值。
#[test]
fn shell_config_debug_hides_the_environment_values() {
    let config = ShellConfig {
        env: vec![("ZCODE_API_KEY".to_owned(), "sk-secret-value".to_owned())].into(),
        ..ShellConfig::default()
    };
    let shown = format!("{config:?}");
    assert!(shown.contains("ZCODE_API_KEY"), "{shown}");
    assert!(!shown.contains("sk-secret-value"), "{shown}");
}
