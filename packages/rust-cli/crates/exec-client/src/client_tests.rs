//! 一条连接上的会话：握手、请求与回复、拒绝、能力位、流的 seq / ack / close、心跳的回应、大消息分片。

use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_exec_transport::fault::Faults;
use rust_cli_exec_wire::dto::command::{CommandEvent, OutputStream};
use rust_cli_exec_wire::dto::core::Outcome;
use rust_cli_exec_wire::dto::files::FileStat;
use rust_cli_exec_wire::envelope::{Ack, Beat, Close, ReplyResult, StreamAck, StreamItem};
use rust_cli_exec_wire::frame::{Kind, Reassembler, Received};
use rust_cli_exec_wire::handshake::{Capabilities, Capability, Refuse, RefuseCode};
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;
use rust_cli_exec_wire::{Body, BodyBuilder};

use crate::test_support::{Peer, network_with, options, welcome, workspace};
use crate::{ClientError, ExecClient};

fn stat() -> Request {
    Request::Stat {
        workspace: workspace(),
        path: "/w/a".to_owned(),
    }
}

fn watch() -> Request {
    Request::WatchCommand {
        operation: "op_1".to_owned(),
        cursor: 0,
    }
}

fn output(text: &str) -> (StreamItem, Body) {
    let mut body = BodyBuilder::new();
    let data = body.push(text.as_bytes());
    (
        StreamItem::Command {
            event: CommandEvent::Output {
                stream: OutputStream::Stdout,
                data,
                cursor: text.len() as u64,
            },
        },
        body.finish(),
    )
}

/// 收 ack，直到这个流 ack 到 `seq`。
async fn acked_through(peer: &mut Peer, call: u64, seq: u64) {
    loop {
        let ack: Ack = peer.next(Kind::Ack).await.header().unwrap();
        if ack.streams.contains(&StreamAck { call, seq }) {
            return;
        }
    }
}

#[tokio::test]
async fn a_call_is_answered_and_its_reply_is_acked() {
    let (connector, listener) = network_with(Vec::new());
    let server = rust_cli_base::owned(async move {
        let mut peer = Peer::accept(&listener).await;
        let hello = peer.handshake("exec-1", Capabilities::ports()).await;
        assert_eq!(hello.epoch, 1);
        assert_eq!(hello.client_instance, "client-1");
        assert_eq!(hello.capabilities, Capabilities::ports());
        assert_eq!(hello.heartbeat_ms, Some(30_000));
        let (header, _) = peer.request().await;
        assert_eq!(header.call, 1);
        assert_eq!(header.request, stat());
        assert_eq!(header.resume_after, None);
        let found = FileStat {
            kind: rust_cli_exec_wire::dto::files::FileKind::File,
            size: 1,
            mtime_ms: 2,
            version: "v".to_owned(),
        };
        peer.reply(1, Reply::Stat(Some(found)), &Body::empty())
            .await;
        let ack: Ack = peer.next(Kind::Ack).await.header().unwrap();
        assert_eq!(ack.replies, [1]);
    });
    let client = ExecClient::connect(connector, options()).await.unwrap();
    assert_eq!(client.welcome().unwrap().server_instance, "exec-1");
    let answer = client.call(stat(), Body::empty()).await.unwrap();
    assert!(matches!(
        answer.result,
        ReplyResult::Ok(Reply::Stat(Some(_)))
    ));
    server.await.unwrap();
    client.close().await;
    assert_eq!(
        client.call(stat(), Body::empty()).await,
        Err(ClientError::Closed)
    );
}

#[tokio::test]
async fn a_refused_handshake_and_another_major_version_fail_to_connect() {
    let (connector, listener) = network_with(Vec::new());
    let server = rust_cli_base::owned(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.next(Kind::Hello).await;
        let refuse = Refuse {
            code: RefuseCode::Auth,
            message: "no token".to_owned(),
        };
        peer.send(Kind::Refuse, &refuse, &Body::empty()).await;
        let mut peer = Peer::accept(&listener).await;
        peer.next(Kind::Hello).await;
        let mut newer = welcome("exec-2", Capabilities::ports());
        newer.protocol = 2;
        peer.send(Kind::Welcome, &newer, &Body::empty()).await;
    });
    assert_eq!(
        ExecClient::connect(connector.clone(), options())
            .await
            .err(),
        Some(ClientError::Refused {
            code: RefuseCode::Auth,
            message: "no token".to_owned()
        })
    );
    assert!(matches!(
        ExecClient::connect(connector, options()).await.err(),
        Some(ClientError::Protocol(_))
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn a_port_the_executor_does_not_report_is_unsupported_without_a_frame() {
    let (connector, listener) = network_with(Vec::new());
    let server = rust_cli_base::owned(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.handshake("exec-1", Capabilities::of([Capability::Files]))
            .await;
        // 第一个到达的请求就是 stat：grep 没有发出来。
        let (header, _) = peer.request().await;
        assert_eq!(header.request, stat());
        peer.reply(header.call, Reply::Stat(None), &Body::empty())
            .await;
    });
    let client = ExecClient::connect(connector, options()).await.unwrap();
    let grep = Request::ListFiles {
        workspace: workspace(),
        cwd: "/w".to_owned(),
    };
    assert_eq!(
        client.call(grep, Body::empty()).await,
        Err(ClientError::Unsupported {
            what: "search.list_files".to_owned()
        })
    );
    client.call(stat(), Body::empty()).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn stream_items_arrive_in_seq_order_without_duplicates_and_are_acked() {
    let (connector, listener) = network_with(Vec::new());
    let server = rust_cli_base::owned(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.handshake("exec-1", Capabilities::ports()).await;
        let (header, _) = peer.request().await;
        assert_eq!(header.request, watch());
        let call = header.call;
        for (seq, text) in [(1, "a"), (1, "a"), (2, "b"), (3, "c")] {
            let (item, body) = output(text);
            peer.event(call, seq, item, &body).await;
        }
        peer.event(call, 4, StreamItem::End, &Body::empty()).await;
        acked_through(&mut peer, call, 4).await;
    });
    let client = ExecClient::connect(connector, options()).await.unwrap();
    let events: Vec<_> = client
        .stream(watch(), Body::empty())
        .await
        .unwrap()
        .collect()
        .await;
    let texts: Vec<String> = events
        .into_iter()
        .map(|event| {
            let event = event.unwrap();
            let StreamItem::Command {
                event: CommandEvent::Output { data, .. },
            } = event.item
            else {
                panic!("not output");
            };
            event.body.text(&data).unwrap()
        })
        .collect();
    assert_eq!(texts, ["a", "b", "c"]);
    server.await.unwrap();
}

#[tokio::test]
async fn dropping_a_stream_before_its_end_closes_it_on_the_executor() {
    let (connector, listener) = network_with(Vec::new());
    let server = rust_cli_base::owned(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.handshake("exec-1", Capabilities::ports()).await;
        let (header, _) = peer.request().await;
        let (item, body) = output("first");
        peer.event(header.call, 1, item, &body).await;
        let close: Close = peer.next(Kind::Close).await.header().unwrap();
        assert_eq!(close.call, header.call);
    });
    let client = ExecClient::connect(connector, options()).await.unwrap();
    let mut stream = client.stream(watch(), Body::empty()).await.unwrap();
    assert!(stream.next().await.unwrap().is_ok());
    drop(stream);
    server.await.unwrap();
}

#[tokio::test]
async fn an_error_item_ends_the_stream_after_it_is_delivered() {
    let (connector, listener) = network_with(Vec::new());
    let server = rust_cli_base::owned(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.handshake("exec-1", Capabilities::ports()).await;
        let (header, _) = peer.request().await;
        let error = rust_cli_exec_wire::envelope::WireError::Unsupported {
            what: "shell_notices".to_owned(),
        };
        peer.event(header.call, 1, StreamItem::Error { error }, &Body::empty())
            .await;
        peer.event(header.call, 2, StreamItem::End, &Body::empty())
            .await;
    });
    let client = ExecClient::connect(connector, options()).await.unwrap();
    let request = Request::ShellNotices {
        session: "sess_1".to_owned(),
        after: 0,
    };
    let items: Vec<_> = client
        .stream(request, Body::empty())
        .await
        .unwrap()
        .collect()
        .await;
    assert_eq!(items.len(), 1);
    assert!(matches!(
        items[0].as_ref().unwrap().item,
        StreamItem::Error { .. }
    ));
    server.await.unwrap();
}

#[tokio::test]
async fn the_client_answers_pings() {
    let (connector, listener) = network_with(vec![Faults::new()]);
    let server = rust_cli_base::owned(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.handshake("exec-1", Capabilities::ports()).await;
        peer.send(Kind::Ping, &Beat { nonce: 7 }, &Body::empty())
            .await;
        let pong: Beat = peer.next(Kind::Pong).await.header().unwrap();
        assert_eq!(pong.nonce, 7);
    });
    let _client = ExecClient::connect(connector, options()).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn large_messages_are_sent_in_parts() {
    let (connector, listener) = network_with(Vec::new());
    let server = rust_cli_base::owned(async move {
        let mut peer = Peer::accept(&listener).await;
        peer.handshake("exec-1", Capabilities::ports()).await;
        // 大消息到了这里是一片一片的：用一个新的拼装器从原始的片里拼回。
        let mut parts = 0;
        let mut reassembler = Reassembler::default();
        let frame = loop {
            let bytes = peer_raw(&mut peer).await;
            if bytes[0] == 0x30 {
                parts += 1;
            }
            if let Received::Frame(frame) = reassembler.push(bytes).unwrap()
                && frame.kind == Kind::Request
            {
                break frame;
            }
        };
        assert!(parts >= 5, "{parts} parts");
        let header: rust_cli_exec_wire::envelope::RequestHeader = frame.header().unwrap();
        let Request::WriteFile { bytes, .. } = header.request else {
            panic!("not a write");
        };
        assert_eq!(frame.body.to_vec(&bytes).unwrap(), vec![9u8; 5000]);
        peer.reply(
            header.call,
            Reply::WriteFile {
                version: "v2".to_owned(),
            },
            &Body::empty(),
        )
        .await;
    });
    let mut small_parts = options();
    small_parts.part_size = 1024;
    let client = ExecClient::connect(connector, small_parts).await.unwrap();
    let mut body = BodyBuilder::new();
    let bytes = body.push(&[9u8; 5000]);
    let write = Request::WriteFile {
        workspace: workspace(),
        context: rust_cli_exec_wire::dto::core::OperationContext {
            environment: "local".to_owned(),
            generation: "g".to_owned(),
            owner_epoch: 1,
            operation: "op_9".to_owned(),
            origin: rust_cli_exec_wire::dto::core::Origin::User { session: None },
        },
        path: "/w/big".to_owned(),
        bytes,
        precondition: rust_cli_exec_wire::dto::files::WritePrecondition::Any,
    };
    let answer = tokio::time::timeout(Duration::from_secs(5), client.call(write, body.finish()))
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        answer.result,
        ReplyResult::Ok(Reply::WriteFile { .. })
    ));
    server.await.unwrap();
    let _ = Outcome::Completed;
}

/// 原始的一段字节（不拼装）。
async fn peer_raw(peer: &mut Peer) -> bytes::Bytes {
    peer.raw().await
}
