//! 流：按 seq 编号、窗口满了就等 ack、乱序的 ack 取最大值、断线后按 `resume_after` 补发、`end` 被 ack 后删掉、`close`
//! 停掉生产者。

use std::sync::{Arc, Mutex};
use std::time::Duration;

use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::envelope::{Ack, Close, EventHeader, RequestHeader, StreamAck, StreamItem};
use rust_cli_exec_wire::frame::Kind;
use rust_cli_exec_wire::handshake::STREAM_WINDOW;
use rust_cli_exec_wire::request::Request;
use rust_cli_kernel_api::env::{CommandEvent, OutputStream};
use tokio::sync::mpsc;

use crate::test_support::{Client, TestMachine, connect, server};

fn watch() -> Request {
    Request::WatchCommand {
        operation: "op_1".to_owned(),
        cursor: 0,
    }
}

fn machine(count: usize, end: bool) -> Arc<TestMachine> {
    let (tx, rx) = mpsc::unbounded_channel();
    for index in 0..count {
        tx.send(Ok(CommandEvent::Output {
            stream: OutputStream::Stdout,
            data: format!("{index}\n").into_bytes(),
            cursor: index as u64,
        }))
        .unwrap();
    }
    if !end {
        // 留着发送端：流不结束。
        std::mem::forget(tx);
    }
    Arc::new(TestMachine {
        events: Mutex::new(Some(rx)),
        ..TestMachine::default()
    })
}

async fn event(client: &mut Client) -> Option<EventHeader> {
    let frame = tokio::time::timeout(Duration::from_millis(300), client.frame())
        .await
        .ok()??;
    assert_eq!(frame.kind, Kind::Event);
    Some(frame.header().unwrap())
}

async fn ack(client: &mut Client, call: u64, seq: u64) {
    let ack = Ack {
        replies: Vec::new(),
        streams: vec![StreamAck { call, seq }],
    };
    client.send(Kind::Ack, &ack, &Body::empty()).await;
}

#[tokio::test]
async fn the_window_holds_unacked_events_and_acks_open_it() {
    let (server, _) = server(machine(70, true)).await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    client.request(1, watch(), &Body::empty()).await;
    let mut seqs = Vec::new();
    while let Some(header) = event(&mut client).await {
        seqs.push(header.seq);
    }
    assert_eq!(seqs, (1..=STREAM_WINDOW).collect::<Vec<_>>());
    // 乱序的 ack：先到 64，再到 10；取最大值，窗口照样打开。
    ack(&mut client, 1, STREAM_WINDOW).await;
    ack(&mut client, 1, 10).await;
    let mut rest = Vec::new();
    while let Some(header) = event(&mut client).await {
        let end = header.item == StreamItem::End;
        rest.push(header.seq);
        if end {
            break;
        }
    }
    assert_eq!(rest, (STREAM_WINDOW + 1..=71).collect::<Vec<_>>());
}

#[tokio::test]
async fn a_resumed_stream_resends_what_was_not_received() {
    let (server, ledger) = server(machine(5, true)).await;
    let (mut client, task) = connect(&server);
    client.handshake(1).await;
    client.request(1, watch(), &Body::empty()).await;
    for expected in 1..=6 {
        assert_eq!(event(&mut client).await.unwrap().seq, expected);
    }
    ack(&mut client, 1, 2).await;
    client.close().await;
    drop(task);
    let (mut again, _task) = connect(&server);
    again.handshake(2).await;
    let header = RequestHeader {
        call: 1,
        request: watch(),
        resume_after: Some(4),
    };
    again.send(Kind::Request, &header, &Body::empty()).await;
    let resent: Vec<u64> = [event(&mut again).await, event(&mut again).await]
        .into_iter()
        .map(|header| header.unwrap().seq)
        .collect();
    assert_eq!(resent, [5, 6]);
    assert!(event(&mut again).await.is_none());
    // ack 到 end（6）：流结束，账本删掉。
    ack(&mut again, 1, 6).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(ledger.is_empty());
}

#[tokio::test]
async fn closing_a_stream_stops_it_and_forgets_it() {
    let (server, ledger) = server(machine(3, false)).await;
    let (mut client, _task) = connect(&server);
    client.handshake(1).await;
    client.request(1, watch(), &Body::empty()).await;
    for _ in 0..3 {
        event(&mut client).await.unwrap();
    }
    assert_eq!(ledger.len(), 1);
    client
        .send(Kind::Close, &Close { call: 1 }, &Body::empty())
        .await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(ledger.is_empty());
    assert!(event(&mut client).await.is_none());
}
