//! 本机 shell 的内部：脚本的拼法、输出所在的私有目录、配置的 Debug；以及其他 shell 测试共用的夹具。经端口可见的行为
//! （输出合流、退出码、超时、取消、cwd 延续与重置、幂等、后台通知、环境投影、进程树回收）在 executor-host 的合同测试里，
//! 对本机与 loopback 远端各跑一遍（docs/specs/rust-cli/remote-exec.md §18.7）。
//! 需要 `/bin/bash`，Windows 上跳过（那里的 Bash 工具要 Git Bash）。
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use futures_util::StreamExt;
use rust_cli_kernel_api::env::{CommandEvent, EnvPath, OperationContext, Origin, StartCommand};
use rust_cli_kernel_api::env_shell::CwdScope;
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
        socket: None,
        env: Default::default(),
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
            CommandEvent::Backgrounded | CommandEvent::BackgroundedBy { .. } => {
                panic!("moved to the background")
            }
        }
    }
    panic!("no terminal event");
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

/// 远端执行 F96（remote-exec.md §23.5）：执行器的服务端按客户端实例填的 `StartCommand.env` 加在投影之后（`None` 是去掉
/// 这个变量）；它不进去重的指纹：同一个操作重放时变量不同也是同一次（不报参数不同）。
#[tokio::test]
async fn command_overrides_are_added_after_the_projection_and_not_fingerprinted() {
    let fixture = fixture(vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        (
            "SSH_AUTH_SOCK".to_owned(),
            "/tmp/ssh-old/agent.1".to_owned(),
        ),
        ("DISPLAY".to_owned(), "localhost:10.0".to_owned()),
    ]);
    let mut start = request(
        r#"printf '%s|%s|%s' "$SSH_AUTH_SOCK" "${DISPLAY-unset}" "$F96_TOKEN""#,
        &fixture.root,
        CwdScope::Pinned,
    );
    start.env = [
        (
            "SSH_AUTH_SOCK".to_owned(),
            Some("/tmp/ssh-new/agent.2".to_owned()),
        ),
        ("DISPLAY".to_owned(), None),
        ("F96_TOKEN".to_owned(), Some("t".to_owned())),
    ]
    .into();
    let ran = run(&fixture, "op_env", start.clone()).await;
    assert_eq!(ran.output, "/tmp/ssh-new/agent.2|unset|t");
    // 重放：变量变了也认出是同一个操作。
    start.env.clear();
    fixture
        .shells
        .start(context("op_env"), start)
        .await
        .unwrap();
}
