//! 环境变量推送（远端执行 F96，remote-exec.md §23.5）：`env.update` 与 `hello.session_env` 按客户端实例生效，只影响之后
//! 启动的命令；保护的名字、保留前缀、不合规则的名字与超过上限的整组都被拒绝；没报告能力位时不执行。

use std::collections::BTreeMap;
use std::sync::Arc;

use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::command::{CwdScope, ShellKind, StartCommand};
use rust_cli_exec_wire::dto::core::{OperationContext, Origin};
use rust_cli_exec_wire::dto::env::{EnvMode, EnvVars};
use rust_cli_exec_wire::envelope::{ReplyResult, WireError};
use rust_cli_exec_wire::frame::Kind;
use rust_cli_exec_wire::handshake::{Capabilities, Hello};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::env_command::EnvOverrides;

use crate::MemoryLedger;
use crate::test_support::{Client, TestMachine, config, connect, hello, server_with, workspace};

fn with_env() -> crate::ServerConfig {
    let mut settings = config();
    settings.capabilities = Capabilities::executor();
    settings
}

fn vars(pairs: &[(&str, Option<&str>)]) -> EnvVars {
    pairs
        .iter()
        .map(|(name, value)| ((*name).to_owned(), value.map(str::to_owned)))
        .collect()
}

fn overrides(pairs: &[(&str, Option<&str>)]) -> EnvOverrides {
    vars(pairs)
}

fn update(mode: EnvMode, pairs: &[(&str, Option<&str>)]) -> Request {
    Request::EnvUpdate {
        mode,
        vars: vars(pairs),
        injected: Vec::new(),
    }
}

fn start(operation: &str) -> Request {
    Request::StartCommand {
        workspace: workspace(),
        context: OperationContext {
            environment: "local".to_owned(),
            generation: "g".to_owned(),
            owner_epoch: 1,
            operation: operation.to_owned(),
            origin: Origin::User { session: None },
        },
        request: Box::new(StartCommand {
            command: "git pull".to_owned(),
            cwd: "/w".to_owned(),
            timeout_ms: None,
            task: format!("t-{operation}"),
            shell: ShellKind::Bash,
            cwd_scope: CwdScope::Pinned,
            background: None,
            sandbox: None,
            hook: None,
            on_timeout: None,
            socket: None,
        }),
    }
}

async fn answer(client: &mut Client, call: u64, request: Request) -> ReplyResult {
    client.request(call, request, &Body::empty()).await;
    loop {
        let (header, _) = client.reply().await;
        if header.call == call {
            return header.result;
        }
    }
}

async fn ok(client: &mut Client, call: u64, request: Request) {
    let result = answer(client, call, request).await;
    assert!(matches!(result, ReplyResult::Ok(_)), "{result:?}");
}

fn bad(result: &ReplyResult, needle: &str) -> bool {
    matches!(result, ReplyResult::Err(WireError::BadRequest { message }) if message.contains(needle))
}

#[tokio::test]
async fn an_update_reaches_the_commands_started_after_it() {
    let machine = Arc::new(TestMachine::default());
    let server = server_with(
        machine.clone(),
        Arc::new(MemoryLedger::default()),
        with_env(),
    )
    .await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    ok(&mut client, 1, start("op1")).await;
    let pushed = [
        ("SSH_AUTH_SOCK", Some("/tmp/ssh-a/agent.1")),
        ("DISPLAY", None),
    ];
    let reply = answer(&mut client, 2, update(EnvMode::Merge, &pushed)).await;
    assert_eq!(reply, ReplyResult::Ok(Reply::EnvUpdate));
    ok(&mut client, 3, start("op2")).await;
    ok(
        &mut client,
        4,
        update(
            EnvMode::Merge,
            &[("SSH_AUTH_SOCK", Some("/tmp/ssh-b/agent.2"))],
        ),
    )
    .await;
    ok(&mut client, 5, start("op3")).await;
    ok(
        &mut client,
        6,
        update(EnvMode::Replace, &[("TOKEN", Some("t"))]),
    )
    .await;
    ok(&mut client, 7, start("op4")).await;
    assert_eq!(
        *machine.started.lock().unwrap(),
        [
            // 推送之前起的命令没有覆盖。
            EnvOverrides::new(),
            overrides(&pushed),
            overrides(&[
                ("SSH_AUTH_SOCK", Some("/tmp/ssh-b/agent.2")),
                ("DISPLAY", None)
            ]),
            overrides(&[("TOKEN", Some("t"))]),
        ]
    );
}

#[tokio::test]
async fn each_client_instance_keeps_its_own_variables() {
    let machine = Arc::new(TestMachine::default());
    let server = server_with(
        machine.clone(),
        Arc::new(MemoryLedger::default()),
        with_env(),
    )
    .await;
    let (mut first, _first) = connect(&server);
    first.handshake(1).await;
    let (mut second, _second) = connect(&server);
    let other = Hello {
        client_instance: "client-2".to_owned(),
        ..hello(1)
    };
    second.send(Kind::Hello, &other, &Body::empty()).await;
    second.frame().await.unwrap();
    ok(
        &mut first,
        1,
        update(EnvMode::Merge, &[("SSH_AUTH_SOCK", Some("/tmp/a"))]),
    )
    .await;
    ok(
        &mut second,
        1,
        update(EnvMode::Merge, &[("SSH_AUTH_SOCK", Some("/tmp/b"))]),
    )
    .await;
    ok(&mut first, 2, start("op-a")).await;
    ok(&mut second, 2, start("op-b")).await;
    let started = machine.started.lock().unwrap().clone();
    assert!(started.contains(&overrides(&[("SSH_AUTH_SOCK", Some("/tmp/a"))])));
    assert!(started.contains(&overrides(&[("SSH_AUTH_SOCK", Some("/tmp/b"))])));
}

#[tokio::test]
async fn the_session_env_of_each_connection_applies_before_its_requests() {
    let machine = Arc::new(TestMachine::default());
    let server = server_with(
        machine.clone(),
        Arc::new(MemoryLedger::default()),
        with_env(),
    )
    .await;
    for (epoch, socket) in [(1, "/tmp/ssh-1/agent.1"), (2, "/tmp/ssh-2/agent.2")] {
        let (mut client, task) = connect(&server);
        let hello = Hello {
            session_env: Some(vars(&[("SSH_AUTH_SOCK", Some(socket))])),
            ..hello(epoch)
        };
        client.send(Kind::Hello, &hello, &Body::empty()).await;
        client.frame().await.unwrap();
        // 紧接着 hello 的第一个请求就看得到这条会话的值。
        ok(&mut client, epoch, start(&format!("op{epoch}"))).await;
        client.close().await;
        let _ = task.await;
    }
    // 这条会话没有这个变量：去掉它，不留上一条会话的。
    let (mut client, _task) = connect(&server);
    let hello = Hello {
        session_env: Some(vars(&[("SSH_AUTH_SOCK", None)])),
        ..hello(3)
    };
    client.send(Kind::Hello, &hello, &Body::empty()).await;
    client.frame().await.unwrap();
    ok(&mut client, 3, start("op3")).await;
    assert_eq!(
        *machine.started.lock().unwrap(),
        [
            overrides(&[("SSH_AUTH_SOCK", Some("/tmp/ssh-1/agent.1"))]),
            overrides(&[("SSH_AUTH_SOCK", Some("/tmp/ssh-2/agent.2"))]),
            overrides(&[("SSH_AUTH_SOCK", None)]),
        ]
    );
}

#[tokio::test]
async fn protected_bad_and_oversized_variables_are_refused() {
    let machine = Arc::new(TestMachine::default());
    let server = server_with(
        machine.clone(),
        Arc::new(MemoryLedger::default()),
        with_env(),
    )
    .await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    ok(
        &mut client,
        1,
        update(EnvMode::Merge, &[("KEEP", Some("1"))]),
    )
    .await;
    let big = "x".repeat(16 * 1024 + 1);
    let many: BTreeMap<String, Option<String>> = (0..65)
        .map(|index| (format!("V{index}"), Some("1".to_owned())))
        .collect();
    let cases: Vec<(Request, &str)> = vec![
        (update(EnvMode::Merge, &[("PATH", Some("/evil"))]), "PATH"),
        (update(EnvMode::Merge, &[("HOME", Some("/evil"))]), "HOME"),
        (
            update(EnvMode::Merge, &[("LD_PRELOAD", Some("x.so"))]),
            "LD_PRELOAD",
        ),
        (
            update(EnvMode::Merge, &[("BASH_FUNC_x%%", Some("()"))]),
            "BASH_FUNC_x%%",
        ),
        (
            update(EnvMode::Merge, &[("_ZCODE_ASSUME_X", Some("1"))]),
            "_ZCODE_ASSUME_X",
        ),
        (update(EnvMode::Merge, &[("1BAD", Some("1"))]), "1BAD"),
        (update(EnvMode::Merge, &[("A=B", Some("1"))]), "A=B"),
        (
            update(EnvMode::Merge, &[("BIG", Some(big.as_str()))]),
            "BIG",
        ),
        (
            Request::EnvUpdate {
                mode: EnvMode::Replace,
                vars: many,
                injected: Vec::new(),
            },
            "64",
        ),
        (
            Request::EnvUpdate {
                mode: EnvMode::Merge,
                vars: vars(&[("TOKEN", Some("t"))]),
                injected: vec!["no good".to_owned()],
            },
            "no good",
        ),
    ];
    for (call, (request, needle)) in (2..).zip(cases) {
        let result = answer(&mut client, call, request).await;
        assert!(bad(&result, needle), "{needle}: {result:?}");
    }
    // 被拒绝的整组什么都没改。
    ok(&mut client, 100, start("op")).await;
    assert_eq!(
        machine.started.lock().unwrap().last(),
        Some(&overrides(&[("KEEP", Some("1"))]))
    );
    // hello 里不合规则的 session_env 只是忽略，连接照常。
    let (mut other, _other) = connect(&server);
    let hello = Hello {
        client_instance: "client-2".to_owned(),
        session_env: Some(vars(&[("PATH", Some("/evil"))])),
        ..hello(1)
    };
    other.send(Kind::Hello, &hello, &Body::empty()).await;
    assert_eq!(other.frame().await.unwrap().kind, Kind::Welcome);
    ok(&mut other, 1, start("op-other")).await;
    assert_eq!(
        machine.started.lock().unwrap().last(),
        Some(&EnvOverrides::new())
    );
}

#[tokio::test]
async fn without_the_capability_the_update_is_unsupported() {
    let machine = Arc::new(TestMachine::default());
    let mut settings = config();
    settings.capabilities = Capabilities::ports_and_mcp();
    let server = server_with(machine.clone(), Arc::new(MemoryLedger::default()), settings).await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    let result = answer(
        &mut client,
        1,
        update(EnvMode::Merge, &[("SSH_AUTH_SOCK", Some("/tmp/a"))]),
    )
    .await;
    assert!(
        matches!(result, ReplyResult::Err(WireError::Unsupported { .. })),
        "{result:?}"
    );
}
