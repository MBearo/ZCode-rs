//! 远端执行 F96 的 Docker 场景（docs/specs/rust-cli/remote-exec.md §23.2、§23.4、§23.5、§23.8）：真实的 sshd 容器、测试专用
//! 的临时密钥与 ssh-agent（不碰用户的 `~/.ssh` 与 agent）。
//!
//! 1. agent 转发：常驻执行器由第一条 ssh 会话拉起；那条会话断了之后，它继承的 `SSH_AUTH_SOCK` 指向的 socket 不在了。
//!    重连之后新起的命令拿到新会话的 `SSH_AUTH_SOCK`（代理在 hello 里带上，执行器按客户端实例用它），socket 在；
//! 2. 撤掉授权的公钥再断线：重连被拒绝（ssh 的 `Permission denied`），状态马上变成 `Failed`，不一直重连。
//!
//! 没有 Docker 或没有 Linux 产物时跳过（同 `ssh_docker.rs`）。容器与镜像用完删掉，ssh-agent 随测试结束。
#![cfg(unix)]
#![allow(
    clippy::disallowed_methods,
    clippy::print_stderr,
    clippy::unwrap_used,
    reason = "集成测试：生成临时密钥、起 Docker 容器与 ssh-agent、拉起 ssh 与 zcode 进程；跳过时写明原因"
)]

mod sshd;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_env_remote::RemoteEnvironment;
use rust_cli_exec_client::{ClientOptions, ConnectionState, Reconnect};
use rust_cli_kernel_api::env::{CommandEvent, EnvPath, StartCommand, WorkspaceRef};
use rust_cli_kernel_api::env_shell::CwdScope;
use rust_cli_kernel_api::ids::{EnvironmentId, OperationId, ShellTaskId};
use rust_cli_kernel_api::machine::MachineEnv;
use rust_cli_machines::{Machine, MachineError, MachineState};
use rust_cli_ssh_connect::{SshProvisioner, SshTarget};
use sshd::{Agent, Sshd, TestEndpoint, VERSION, artifact_or_skip, context, docker, wanted};

/// 在 `/tmp` 下跑一条命令，取它的输出（等到终态）。
async fn output(machine: &MachineEnv, operation: &str, text: &str) -> String {
    let ws = WorkspaceRef {
        environment: EnvironmentId::new("dev-a"),
        root: EnvPath::new("/tmp"),
    };
    let start = StartCommand {
        command: text.to_owned(),
        cwd: EnvPath::new("/tmp"),
        timeout_ms: Some(30_000),
        task: ShellTaskId::new(format!("t-{operation}")),
        shell: Default::default(),
        cwd_scope: CwdScope::Pinned,
        background: None,
        sandbox: None,
        hook: None,
        on_timeout: None,
        socket: None,
        env: Default::default(),
    };
    machine
        .commands()
        .start_command(&ws, context(operation), start)
        .await
        .unwrap();
    let mut events = machine
        .commands()
        .watch_command(OperationId::new(operation), 0);
    let mut text = String::new();
    while let Some(event) = tokio::time::timeout(Duration::from_secs(30), events.next())
        .await
        .unwrap()
    {
        match event.unwrap() {
            CommandEvent::Output { data, .. } => text.push_str(&String::from_utf8_lossy(&data)),
            CommandEvent::Finished { .. } => break,
            _ => {}
        }
    }
    text
}

/// `SSH_AUTH_SOCK` 的值，与它指向的 socket 在不在。
async fn agent_socket(machine: &MachineEnv, operation: &str) -> (String, bool) {
    let text = output(
        machine,
        operation,
        r#"printf '%s ' "$SSH_AUTH_SOCK"; if test -S "$SSH_AUTH_SOCK"; then echo alive; else echo gone; fi"#,
    )
    .await;
    let (path, state) = text.trim().rsplit_once(' ').unwrap_or((text.trim(), ""));
    (path.to_owned(), state == "alive")
}

/// 等这台机器的状态满足条件（最多 60 s）。
async fn until(machine: &Machine, done: impl Fn(&MachineState) -> bool) -> MachineState {
    let mut states = machine.states();
    tokio::time::timeout(
        Duration::from_secs(60),
        states.wait_for(|state| done(state)),
    )
    .await
    .unwrap_or_else(|_| panic!("the machine stayed {:?}", machine.state()))
    .unwrap()
    .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reconnect_refreshes_ssh_auth_sock_and_a_revoked_key_fails_at_once() {
    let Some(artifact) = artifact_or_skip() else {
        return;
    };
    let sshd = Sshd::start();
    sshd.wait_ready().await;
    let agent = Agent::start(&sshd).await;
    let config: PathBuf = sshd.forwarding_config(&agent);
    let target = SshTarget {
        config: Some(config.clone()),
        ..sshd.target()
    };
    let endpoint = Arc::new(TestEndpoint {
        target: target.clone(),
        provisioner: SshProvisioner::new(target, wanted(), vec![artifact]),
    });
    let mut options = ClientOptions::new(VERSION, "agent-test");
    // 同产品的一直重连（F96），间隔缩短。
    options.reconnect = Reconnect {
        attempts: None,
        delay: Duration::from_millis(100),
        max_delay: Duration::from_millis(1000),
    };
    let machine = Machine::new(endpoint, options);
    let client = machine.connect().await.unwrap();
    assert!(
        client
            .welcome()
            .unwrap()
            .capabilities
            .contains(rust_cli_exec_wire::handshake::Capability::Env)
    );
    let remote = Arc::new(RemoteEnvironment::new(client.clone()).with_name("dev-a")).machine();

    // 1. 第一条会话：agent 转发的 socket 在。
    let (first, alive) = agent_socket(&remote, "op_first").await;
    assert!(first.starts_with("/tmp/ssh-"), "{first}");
    assert!(alive, "the forwarded agent works on the first connection");

    // 断线（杀掉本机的 ssh），客户端经新的 ssh 重连到同一个常驻执行器。
    let killed = sshd.kill_ssh_using(&config.to_string_lossy());
    assert!(killed >= 1);
    let mut states = client.states();
    tokio::time::timeout(
        Duration::from_secs(60),
        states.wait_for(
            |state| matches!(state, ConnectionState::Connected { epoch, .. } if *epoch >= 2),
        ),
    )
    .await
    .unwrap()
    .unwrap();
    // 第一条会话的 socket 被 sshd 删掉了（常驻执行器继承的就是它，§23.2）。
    let (ok, gone, err) = sshd.ssh(&format!("test -S {first} && echo alive || echo gone"));
    assert!(ok, "{err}");
    assert_eq!(gone.trim(), "gone");

    // 2. 重连后新起的命令拿到新会话的 SSH_AUTH_SOCK，socket 在。旧代码在这里是第一条会话的路径、socket 不在。
    let (second, alive) = agent_socket(&remote, "op_second").await;
    assert_ne!(
        second, first,
        "the command got the new session's agent socket"
    );
    assert!(alive, "the forwarded agent works after the reconnect");

    // 3. 撤掉授权的公钥再断线：重连被拒绝，马上 Failed（不一直重连）。
    let (ok, _, err) = docker(&["exec", &sshd.name, "rm", "/root/.ssh/authorized_keys"]);
    assert!(ok, "{err}");
    sshd.kill_ssh_using(&config.to_string_lossy());
    let failed = until(&machine, |state| {
        matches!(state, MachineState::Failed { .. })
    })
    .await;
    assert!(
        matches!(&failed, MachineState::Failed { error: MachineError::Unreachable(message) } if message.contains("Permission denied")),
        "{failed:?}"
    );

    // 收尾：停掉容器里的常驻执行器（容器随后删掉）。
    let installed = format!("/root/.zcode/rust-cli/exec/bin/{VERSION}/zcode-rs");
    let (ok, out, err) = docker(&["exec", &sshd.name, &installed, "executor", "stop"]);
    assert!(ok, "{out}{err}");
    drop(remote);
    machine.disconnect().await;
    drop(agent);
}
