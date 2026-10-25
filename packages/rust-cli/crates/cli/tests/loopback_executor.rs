//! 执行器模式，真实进程（docs/specs/rust-cli/remote-exec.md §18.6、§18.7、§19.3）：经 stdio 拉起
//! `zcode executor --stdio --standalone`（loopback 的形式：每个会话一个执行器，不经常驻进程），
//! 用 `RemoteEnvironment` 读写文件、跑命令；客户端关闭后执行器退出，并结束它起的后台命令。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::unwrap_used,
    reason = "集成测试：准备临时目录、拉起执行器进程、检查进程是否还在"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ExecClient};
use rust_cli_exec_transport::stdio::{ChildCommand, StdioConnector};
use rust_cli_exec_wire::handshake::Capabilities;
use rust_cli_kernel_api::env::{
    CommandEvent, EnvPath, OperationContext, Origin, ReadFile, StartCommand, WorkspaceRef,
    WriteFile, WritePrecondition,
};
use rust_cli_kernel_api::env_shell::{BackgroundCommand, CwdScope};
use rust_cli_kernel_api::ids::{
    EnvironmentId, Generation, OperationId, OwnerEpoch, SessionId, ShellTaskId,
};

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

fn command(root: &std::path::Path, task: &str, text: &str) -> StartCommand {
    StartCommand {
        command: text.to_owned(),
        cwd: EnvPath::new(root.to_string_lossy()),
        timeout_ms: Some(10_000),
        task: ShellTaskId::new(task),
        shell: Default::default(),
        cwd_scope: CwdScope::Pinned,
        background: None,
        sandbox: None,
        hook: None,
        on_timeout: None,
        socket: None,
        env: Default::default(),
    }
}

/// home 下名字带 `model-io` 的文件。
fn model_io_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.to_string_lossy().contains("model-io") {
                found.push(path);
            }
        }
    }
    found
}

fn alive(pid: &str) -> bool {
    std::process::Command::new("/bin/kill")
        .args(["-0", pid])
        .stderr(std::process::Stdio::null())
        .status()
        .unwrap()
        .success()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_executor_serves_the_ports_over_stdio_and_exits_when_closed() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let temp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(work.path()).unwrap();
    let env = vec![
        (
            "HOME".to_owned(),
            home.path().to_string_lossy().into_owned(),
        ),
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("SHELL".to_owned(), "/bin/bash".to_owned()),
        (
            "TMPDIR".to_owned(),
            temp.path().to_string_lossy().into_owned(),
        ),
    ];
    let connector = StdioConnector::new(ChildCommand {
        program: PathBuf::from(env!("CARGO_BIN_EXE_zcode-rs")),
        args: ["executor", "--stdio", "--standalone", "--add-dir", "/extra"]
            .map(str::to_owned)
            .to_vec(),
        env,
        cwd: Some(root.clone()),
        kill_on_drop: false,
    });
    let client = ExecClient::connect(
        Arc::new(connector),
        ClientOptions::new("test", "loopback-test"),
    )
    .await
    .unwrap();
    let welcome = client.welcome().unwrap();
    assert_eq!(welcome.os, std::env::consts::OS);
    // R5（remote-exec.md §22.4）：执行器也报告 MCP 的管道；F96（§23.5）：还有环境变量推送。
    assert_eq!(welcome.capabilities, Capabilities::executor());
    assert_eq!(welcome.shell.as_deref(), Some("bash"));
    let remote = Arc::new(RemoteEnvironment::new(client.clone()));
    let machine = remote.clone().machine();
    let ws = WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root.to_string_lossy()),
    };
    let facts = machine.info().facts(&ws).await.unwrap();
    assert_eq!(facts.additional_directories, [EnvPath::new("/extra")]);
    assert_eq!(facts.cwd, ws.root);

    let path = EnvPath::new(root.join("a.txt").to_string_lossy());
    let write = WriteFile {
        path: path.clone(),
        bytes: b"hello".to_vec(),
        precondition: WritePrecondition::Absent,
    };
    machine
        .files()
        .write_file(&ws, context("op_w"), write)
        .await
        .unwrap();
    let read = machine
        .files()
        .read_file(&ws, ReadFile { path })
        .await
        .unwrap();
    assert_eq!(read.bytes, b"hello");

    // 一条后台命令：记下 sleep 的 pid，执行器退出时它要被结束。
    let mut background = command(&root, "bloop1", "echo $$; exec sleep 30");
    background.background = Some(BackgroundCommand {
        description: "sleep".to_owned(),
        tool_use: None,
        kind: Default::default(),
    });
    machine
        .commands()
        .start_command(&ws, context("op_bg"), background)
        .await
        .unwrap();
    let mut events = machine
        .commands()
        .watch_command(OperationId::new("op_bg"), 0);
    let mut pid = String::new();
    while let Some(event) = events.next().await {
        if let CommandEvent::Output { data, .. } = event.unwrap() {
            pid.push_str(&String::from_utf8_lossy(&data));
            if pid.ends_with('\n') {
                break;
            }
        }
    }
    drop(events);
    let pid = pid.trim().to_owned();
    assert!(alive(&pid), "the background command runs");

    let ran = command(&root, "bloop2", "echo hi");
    machine
        .commands()
        .start_command(&ws, context("op_fg"), ran)
        .await
        .unwrap();
    let mut events = machine
        .commands()
        .watch_command(OperationId::new("op_fg"), 0);
    let mut output = Vec::new();
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Output { data, .. } => output.extend(data),
            CommandEvent::Finished { exit_code, .. } => {
                assert_eq!(exit_code, Some(0));
                break;
            }
            _ => {}
        }
    }
    assert_eq!(output, b"hi\n");

    // F96（remote-exec.md §23.5）：推一组变量，之后起的命令看得到；保护的名字被拒绝。
    remote
        .update_env(
            rust_cli_exec_wire::dto::env::EnvMode::Merge,
            [("F96_MARK".to_owned(), Some("pushed".to_owned()))].into(),
            Vec::new(),
        )
        .await
        .unwrap();
    let refused = remote
        .update_env(
            rust_cli_exec_wire::dto::env::EnvMode::Merge,
            [("PATH".to_owned(), Some("/evil".to_owned()))].into(),
            Vec::new(),
        )
        .await;
    assert!(
        matches!(&refused, Err(rust_cli_kernel_api::env::EnvError::Io { message }) if message.contains("PATH")),
        "{refused:?}"
    );
    let marked = command(&root, "bloop3", r#"printf '%s' "$F96_MARK""#);
    machine
        .commands()
        .start_command(&ws, context("op_env"), marked)
        .await
        .unwrap();
    let mut events = machine
        .commands()
        .watch_command(OperationId::new("op_env"), 0);
    let mut output = Vec::new();
    while let Some(event) = events.next().await {
        match event.unwrap() {
            CommandEvent::Output { data, .. } => output.extend(data),
            CommandEvent::Finished { .. } => break,
            _ => {}
        }
    }
    assert_eq!(output, b"pushed");

    drop(machine);
    drop(remote);
    client.close().await;
    // 执行器不发模型请求，也不建模型轨迹的写者（remote-exec.md §18.6）：home 下没有 model-io 记录。
    assert!(model_io_files(home.path()).is_empty());
    // 执行器读到 stdin 结束就退出，退出时结束它起的命令。
    for _ in 0..200 {
        if !alive(&pid) {
            return;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    panic!("the background command outlived the executor");
}
