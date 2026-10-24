//! 握手（主版本、鉴权、旧 epoch）、先记账再执行、重复调用不重跑、同一个 ID 不同内容、ack 后删掉、断线不取消、围栏、
//! 不认识的消息与没报告的端口、账本恢复出的中断、心跳的回应、被新连接替换的旧连接。

use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use rust_cli_exec_wire::envelope::{Ack, Beat, ReplyResult, WireError};
use rust_cli_exec_wire::frame::Kind;
use rust_cli_exec_wire::handshake::{Capabilities, Capability, Hello, Refuse, RefuseCode, Welcome};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_exec_wire::{Body, BodyBuilder};
use tokio::sync::Notify;

use crate::test_support::{
    TestMachine, config, connect, hello, server, server_with, workspace, write,
};
use crate::{
    Authenticator, Begun, CallKey, CallKind, ExecLedger, ExecServer, Fence, LedgerError,
    MemoryLedger, Principal,
};

fn read(path: &str) -> Request {
    Request::ReadFile {
        workspace: workspace(),
        path: path.to_owned(),
    }
}

#[tokio::test]
async fn the_handshake_reports_the_executor_and_refuses_another_major_version() {
    let (server, _) = server(Arc::default()).await;
    let (mut client, _task) = connect(&server);
    let welcome: Welcome = client.handshake(1).await.header().unwrap();
    assert_eq!(welcome.server_instance, server.instance());
    assert_eq!(
        (welcome.os.as_str(), welcome.arch.as_str()),
        ("linux", "x86_64")
    );
    assert_eq!(welcome.capabilities, Capabilities::ports());
    assert_eq!(welcome.home.as_deref(), Some("/home/dev"));

    let (mut newer, _task) = connect(&server);
    let mut future = hello(1);
    future.protocol = 2;
    newer.send(Kind::Hello, &future, &Body::empty()).await;
    let refused = newer.frame().await.unwrap();
    assert_eq!(refused.kind, Kind::Refuse);
    assert_eq!(
        refused.header::<Refuse>().unwrap().code,
        RefuseCode::Protocol
    );
    assert!(newer.frame().await.is_none());
}

struct Deny;

#[async_trait]
impl Authenticator for Deny {
    async fn authenticate(&self, _hello: &Hello) -> Result<Principal, String> {
        Err("token expired".to_owned())
    }
}

#[tokio::test]
async fn an_authenticator_can_refuse_and_an_older_epoch_is_stale() {
    let machine = rust_cli_kernel_api::machine::MachineEnv::new(
        Arc::new(TestMachine::default()),
        Arc::new(rust_cli_kernel_api::mcp::NoMcpTransport),
    );
    let denied = ExecServer::start(
        machine,
        Arc::new(MemoryLedger::default()),
        Arc::new(Deny),
        config(),
    )
    .await
    .unwrap();
    let (mut client, _task) = connect(&denied);
    let refuse: Refuse = client.handshake(1).await.header().unwrap();
    assert_eq!(
        (refuse.code, refuse.message.as_str()),
        (RefuseCode::Auth, "token expired")
    );

    let (server, _) = server(Arc::default()).await;
    let (mut second, _task2) = connect(&server);
    assert_eq!(second.handshake(2).await.kind, Kind::Welcome);
    let (mut first, _task1) = connect(&server);
    let stale: Refuse = first.handshake(1).await.header().unwrap();
    assert_eq!(stale.code, RefuseCode::Stale);
}

#[tokio::test]
async fn a_repeated_call_is_answered_from_the_ledger_and_runs_once() {
    let machine = Arc::new(TestMachine::default());
    let (server, ledger) = server(machine.clone()).await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    let mut body = BodyBuilder::new();
    let request = write("/w/a", &mut body, b"abc");
    let body = body.finish();
    client.request(7, request.clone(), &body).await;
    let (first, _) = client.reply().await;
    client.request(7, request, &body).await;
    let (again, _) = client.reply().await;
    assert_eq!(first, again);
    assert_eq!(machine.writes.load(Ordering::SeqCst), 1);
    // 同一个调用 ID、不同的内容：拒绝。
    let mut other = BodyBuilder::new();
    let changed = write("/w/a", &mut other, b"zzz");
    client.request(7, changed, &other.finish()).await;
    let (conflict, _) = client.reply().await;
    assert!(matches!(
        conflict.result,
        ReplyResult::Err(WireError::OperationConflict { .. })
    ));
    assert_eq!(ledger.len(), 1);
    client
        .send(
            Kind::Ack,
            &Ack {
                replies: vec![7],
                streams: Vec::new(),
            },
            &Body::empty(),
        )
        .await;
    client.request(8, read("/w/b"), &Body::empty()).await;
    let (read, body) = client.reply().await;
    let ReplyResult::Ok(Reply::ReadFile(content)) = read.result else {
        panic!("not a read");
    };
    assert_eq!(body.text(&content.bytes).unwrap(), "content of /w/b");
    assert_eq!(
        ledger.len(),
        1,
        "the acked call was forgotten, the new one is kept"
    );
}

#[tokio::test]
async fn a_disconnect_does_not_cancel_and_the_reply_is_fetched_after_reconnecting() {
    let gate = Arc::new(Notify::new());
    let machine = Arc::new(TestMachine {
        gate: Some(gate.clone()),
        ..TestMachine::default()
    });
    let (server, _) = server(machine.clone()).await;
    let (mut client, task) = connect(&server);
    client.handshake(1).await;
    let mut body = BodyBuilder::new();
    let request = write("/w/slow", &mut body, b"x");
    let body = body.finish();
    client.request(1, request.clone(), &body).await;
    tokio::time::sleep(Duration::from_millis(20)).await;
    client.close().await;
    drop(task);
    gate.notify_one();
    // 等执行完成（回复只进账本：没有连接）。
    for _ in 0..100 {
        if machine.writes.load(Ordering::SeqCst) == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let (mut again, _task) = connect(&server);
    again.handshake(2).await;
    again.request(1, request, &body).await;
    let (reply, _) = again.reply().await;
    assert!(matches!(
        reply.result,
        ReplyResult::Ok(Reply::WriteFile { .. })
    ));
    assert_eq!(machine.writes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn the_fence_keeps_requests_inside_the_roots() {
    let mut fenced = config();
    fenced.fence = Fence::Roots(vec!["/w".to_owned()]);
    let (server, _) = (
        server_with(Arc::default(), Arc::new(MemoryLedger::default()), fenced).await,
        (),
    );
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    for (call, path, allowed) in [
        (1, "/w/a", true),
        (2, "/etc/passwd", false),
        (3, "/w/../etc/passwd", false),
        (4, "/w2/a", false),
        (5, "relative", false),
        (6, "/w/./b/../c", true),
    ] {
        client.request(call, read(path), &Body::empty()).await;
        let (reply, _) = client.reply().await;
        let denied = matches!(
            reply.result,
            ReplyResult::Err(WireError::PermissionDenied { .. })
        );
        assert_eq!(!denied, allowed, "{path}");
    }
}

#[tokio::test]
async fn unknown_messages_and_unreported_ports_are_unsupported() {
    let mut files_only = config();
    files_only.capabilities = Capabilities::of([Capability::Files]);
    let server = server_with(
        Arc::default(),
        Arc::new(MemoryLedger::default()),
        files_only,
    )
    .await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    let future = serde_json::json!({"call": 3, "request": {"op": "files.teleport", "args": {}}});
    client.send(Kind::Request, &future, &Body::empty()).await;
    let (reply, _) = client.reply().await;
    assert_eq!(reply.call, 3);
    assert!(matches!(
        reply.result,
        ReplyResult::Err(WireError::Unsupported { .. })
    ));
    let search = Request::ListFiles {
        workspace: workspace(),
        cwd: "/w".to_owned(),
    };
    client.request(4, search, &Body::empty()).await;
    let (reply, _) = client.reply().await;
    assert_eq!(
        reply.result,
        ReplyResult::Err(WireError::Unsupported {
            what: "search.list_files".to_owned()
        })
    );
}

/// 恢复出“执行器重启前没有终态”的账本。
#[derive(Default)]
struct Restarted;

#[async_trait]
impl ExecLedger for Restarted {
    async fn begin(&self, _: &CallKey, _: &str, _: CallKind) -> Result<Begun, LedgerError> {
        Ok(Begun::Interrupted)
    }
    async fn finish(&self, _: &CallKey, _: Bytes) -> Result<(), LedgerError> {
        Ok(())
    }
    async fn append_event(&self, _: &CallKey, _: u64, _: Bytes) -> Result<(), LedgerError> {
        Ok(())
    }
    async fn ack_events(&self, _: &CallKey, _: u64) -> Result<(), LedgerError> {
        Ok(())
    }
    async fn events_after(&self, _: &CallKey, _: u64) -> Result<Vec<Bytes>, LedgerError> {
        Ok(Vec::new())
    }
    async fn forget(&self, _: &CallKey) -> Result<(), LedgerError> {
        Ok(())
    }
    async fn recover(&self) -> Result<Vec<CallKey>, LedgerError> {
        Ok(vec![CallKey {
            instance: "client-1".to_owned(),
            call: 1,
        }])
    }
}

#[tokio::test]
async fn a_call_left_unfinished_before_a_restart_is_interrupted_not_rerun() {
    let machine = Arc::new(TestMachine::default());
    let server = server_with(machine.clone(), Arc::new(Restarted), config()).await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    let mut body = BodyBuilder::new();
    let request = write("/w/a", &mut body, b"1");
    client.request(1, request, &body.finish()).await;
    let (reply, _) = client.reply().await;
    assert!(matches!(
        reply.result,
        ReplyResult::Err(WireError::Interrupted { .. })
    ));
    assert_eq!(machine.writes.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn pings_are_answered_and_a_replaced_connection_stops_serving() {
    let (server, _) = server(Arc::default()).await;
    let (mut old, old_task) = connect(&server);
    old.handshake(1).await;
    old.send(Kind::Ping, &Beat { nonce: 3 }, &Body::empty())
        .await;
    let pong = old.frame().await.unwrap();
    assert_eq!(
        (pong.kind, pong.header::<Beat>().unwrap().nonce),
        (Kind::Pong, 3)
    );
    let (mut new, _new_task) = connect(&server);
    new.handshake(2).await;
    // 旧连接上的帧作废：服务这条连接的任务结束。
    old.request(1, read("/w/a"), &Body::empty()).await;
    tokio::time::timeout(Duration::from_secs(5), old_task)
        .await
        .unwrap()
        .unwrap();
    new.request(2, read("/w/a"), &Body::empty()).await;
    assert_eq!(new.reply().await.0.call, 2);
}
