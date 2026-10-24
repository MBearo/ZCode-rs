//! 常驻执行器要的服务端能力（remote-exec.md §19.5、§19.7、§19.8）：此刻的活动（空闲判断）、断开太久的实例被清掉、
//! `welcome.ledger` 来自账本的身份、账本收到调用的种类。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use rust_cli_exec_wire::handshake::Welcome;
use rust_cli_exec_wire::request::Request;
use rust_cli_exec_wire::{Body, BodyBuilder};
use tokio::sync::Notify;

use crate::test_support::{TestMachine, config, connect, server_with, write};
use crate::{Activity, Begun, CallKey, CallKind, ExecLedger, LedgerError, MemoryLedger};

/// 记下每次 `begin` 的种类、有身份的内存账本。
#[derive(Default)]
struct Recording {
    inner: MemoryLedger,
    kinds: Mutex<Vec<CallKind>>,
    forgotten: Mutex<Vec<String>>,
}

#[async_trait]
impl ExecLedger for Recording {
    async fn begin(&self, key: &CallKey, fp: &str, kind: CallKind) -> Result<Begun, LedgerError> {
        self.kinds.lock().unwrap().push(kind);
        self.inner.begin(key, fp, kind).await
    }
    async fn finish(&self, key: &CallKey, reply: Bytes) -> Result<(), LedgerError> {
        self.inner.finish(key, reply).await
    }
    async fn append_event(&self, key: &CallKey, seq: u64, event: Bytes) -> Result<(), LedgerError> {
        self.inner.append_event(key, seq, event).await
    }
    async fn ack_events(&self, key: &CallKey, seq: u64) -> Result<(), LedgerError> {
        self.inner.ack_events(key, seq).await
    }
    async fn events_after(&self, key: &CallKey, seq: u64) -> Result<Vec<Bytes>, LedgerError> {
        self.inner.events_after(key, seq).await
    }
    async fn forget(&self, key: &CallKey) -> Result<(), LedgerError> {
        self.inner.forget(key).await
    }
    async fn recover(&self) -> Result<Vec<CallKey>, LedgerError> {
        Ok(Vec::new())
    }
    fn identity(&self) -> Option<String> {
        Some("ledger-1".to_owned())
    }
    async fn forget_client(&self, instance: &str) -> Result<(), LedgerError> {
        self.forgotten.lock().unwrap().push(instance.to_owned());
        self.inner.forget_client(instance).await
    }
}

fn watch() -> Request {
    Request::WatchCommand {
        operation: "op_1".to_owned(),
        cursor: 0,
    }
}

#[tokio::test]
async fn activity_counts_clients_running_calls_and_open_streams() {
    let gate = Arc::new(Notify::new());
    let machine = Arc::new(TestMachine {
        gate: Some(gate.clone()),
        ..TestMachine::default()
    });
    let ledger = Arc::new(Recording::default());
    let server = server_with(machine, ledger.clone(), config()).await;
    assert_eq!(server.activity(), Activity::default());
    let (mut client, task) = connect(&server);
    let welcome: Welcome = client.handshake(1).await.header().unwrap();
    assert_eq!(welcome.ledger.as_deref(), Some("ledger-1"));
    let mut body = BodyBuilder::new();
    let request = write("/w/a", &mut body, b"x");
    client.request(1, request, &body.finish()).await;
    client.request(2, watch(), &Body::empty()).await;
    for _ in 0..100 {
        if server.activity().calls == 1 && server.activity().streams == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(
        server.activity(),
        Activity {
            clients: 1,
            calls: 1,
            streams: 1
        }
    );
    gate.notify_one();
    let _ = client.reply().await;
    assert_eq!(server.activity().calls, 0);
    client.close().await;
    let _ = task.await;
    assert_eq!(server.activity().clients, 0);
    // 账本收到了种类：变更类与流（只读调用在别的测试里）。
    assert_eq!(
        *ledger.kinds.lock().unwrap(),
        [CallKind::Mutation, CallKind::Stream]
    );
}

#[tokio::test]
async fn a_client_away_too_long_loses_its_streams_and_memory_records() {
    let mut settings = config();
    settings.abandon_after = Duration::from_millis(40);
    settings.sweep_every = Duration::from_millis(10);
    let ledger = Arc::new(Recording::default());
    let server = server_with(Arc::new(TestMachine::default()), ledger.clone(), settings).await;
    let (mut client, task) = connect(&server);
    client.handshake(1).await;
    client.request(1, watch(), &Body::empty()).await;
    for _ in 0..100 {
        if server.activity().streams == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(server.activity().streams, 1);
    client.close().await;
    let _ = task.await;
    for _ in 0..200 {
        if server.activity().streams == 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    assert_eq!(server.activity().streams, 0);
    assert_eq!(*ledger.forgotten.lock().unwrap(), ["client-1"]);
}

#[tokio::test]
async fn a_client_that_comes_back_in_time_is_kept() {
    let mut settings = config();
    settings.abandon_after = Duration::from_secs(3600);
    settings.sweep_every = Duration::from_millis(5);
    let ledger = Arc::new(Recording::default());
    let server = server_with(Arc::new(TestMachine::default()), ledger.clone(), settings).await;
    let (mut client, task) = connect(&server);
    client.handshake(1).await;
    client.request(1, watch(), &Body::empty()).await;
    client.close().await;
    let _ = task.await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(ledger.forgotten.lock().unwrap().is_empty());
    let read = Request::ReadFile {
        workspace: crate::test_support::workspace(),
        path: "/w/a".to_owned(),
    };
    let (mut again, _task) = connect(&server);
    again.handshake(2).await;
    again.request(2, read, &Body::empty()).await;
    let _ = again.reply().await;
    assert_eq!(ledger.kinds.lock().unwrap().last(), Some(&CallKind::Read));
}
