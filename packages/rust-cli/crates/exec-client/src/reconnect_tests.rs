//! 断线与重连（remote-exec.md §18.5）：心跳判死、同一个调用 ID 补发、流按 `resume_after` 续上、执行器换了实例时有副作用
//! 的调用以 Interrupted 结束、重连失败时以 Disconnected 结束。

use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_exec_transport::fault::Faults;
use rust_cli_exec_wire::Body;
use rust_cli_exec_wire::dto::files::{PutFile, WritePrecondition};
use rust_cli_exec_wire::envelope::{ReplyResult, StreamItem};
use rust_cli_exec_wire::handshake::Capabilities;
use rust_cli_exec_wire::reply::Reply;
use rust_cli_exec_wire::request::Request;

use crate::test_support::{Peer, network_with, reconnecting, welcome, workspace};
use crate::{ClientError, ConnectionState, ExecClient, Reconnect, backoff};

fn stat() -> Request {
    Request::Stat {
        workspace: workspace(),
        path: "/w/a".to_owned(),
    }
}

fn put() -> Request {
    Request::PutFile {
        workspace: workspace(),
        context: rust_cli_exec_wire::dto::core::OperationContext {
            environment: "local".to_owned(),
            generation: "g".to_owned(),
            owner_epoch: 1,
            operation: "op_put".to_owned(),
            origin: rust_cli_exec_wire::dto::core::Origin::User { session: None },
        },
        request: PutFile {
            path: "/w/a".to_owned(),
            bytes: rust_cli_exec_wire::Blob { at: 0, len: 0 },
            precondition: WritePrecondition::Any,
            create_parents: false,
        },
    }
}

fn notices() -> Request {
    Request::ShellNotices {
        session: "sess_1".to_owned(),
        after: 0,
    }
}

#[tokio::test]
async fn a_silent_executor_is_detected_and_the_call_is_resent_with_the_same_id() {
    let (connector, listener) = network_with(Vec::new());
    let server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        let (header, _) = first.request().await;
        assert_eq!(header.call, 1);
        // 不回复、不回 pong：客户端三个心跳间隔后判它断了，重连。
        let mut second = Peer::accept(&listener).await;
        let hello = second.handshake("exec-1", Capabilities::ports()).await;
        assert_eq!(hello.epoch, 2);
        let (again, _) = second.request().await;
        assert_eq!((again.call, again.request), (1, stat()));
        second.reply(1, Reply::Stat(None), &Body::empty()).await;
        drop(first);
    });
    let client = ExecClient::connect(connector, reconnecting(20))
        .await
        .unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(5), client.call(stat(), Body::empty()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer.result, ReplyResult::Ok(Reply::Stat(None)));
    server.await.unwrap();
}

#[tokio::test]
async fn a_stream_resumes_after_the_last_received_seq_on_the_same_executor() {
    let (connector, listener) = network_with(Vec::new());
    let switches = connector.clone();
    let server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        let (header, _) = first.request().await;
        assert_eq!(header.resume_after, None);
        for seq in [1, 2] {
            let item = StreamItem::Error {
                error: rust_cli_exec_wire::envelope::WireError::Io {
                    message: format!("item {seq}"),
                },
            };
            first.event(header.call, seq, item, &Body::empty()).await;
        }
        // 等客户端收到两项再切断。
        tokio::time::sleep(Duration::from_millis(50)).await;
        switches.switch(0).cut();
        let mut second = Peer::accept(&listener).await;
        second.handshake("exec-1", Capabilities::ports()).await;
        let (again, _) = second.request().await;
        assert_eq!((again.call, again.resume_after), (header.call, Some(2)));
        second
            .event(header.call, 3, StreamItem::End, &Body::empty())
            .await;
    });
    let client = ExecClient::connect(connector, reconnecting(1000))
        .await
        .unwrap();
    let mut stream = client.stream(notices(), Body::empty()).await.unwrap();
    // 流里的错误项交给消费方后流结束：这里只看第一项，然后丢掉；第二个流验证续上。
    assert!(stream.next().await.unwrap().is_ok());
    let rest: Vec<_> = tokio::time::timeout(Duration::from_secs(5), stream.collect::<Vec<_>>())
        .await
        .unwrap();
    assert_eq!(rest.len(), 1);
    server.await.unwrap();
}

#[tokio::test]
async fn after_the_executor_restarts_sent_mutations_are_interrupted_and_reads_are_resent() {
    let (connector, listener) = network_with(Vec::new());
    let switches = connector.clone();
    let server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        let (a, _) = first.request().await;
        let (b, _) = first.request().await;
        let mut calls = [a.call, b.call];
        calls.sort_unstable();
        switches.switch(0).cut();
        let mut second = Peer::accept(&listener).await;
        second.handshake("exec-2", Capabilities::ports()).await;
        let (again, _) = second.request().await;
        assert_eq!(again.request, stat());
        second
            .reply(again.call, Reply::Stat(None), &Body::empty())
            .await;
    });
    let client = ExecClient::connect(connector, reconnecting(1000))
        .await
        .unwrap();
    let (written, read) = tokio::join!(
        client.call(put(), Body::empty()),
        client.call(stat(), Body::empty())
    );
    assert_eq!(
        written,
        Err(ClientError::Interrupted {
            op: "files.put_file".to_owned()
        })
    );
    assert_eq!(read.unwrap().result, ReplyResult::Ok(Reply::Stat(None)));
    server.await.unwrap();
}

#[tokio::test]
async fn when_reconnecting_fails_pending_and_later_calls_end_disconnected() {
    let (connector, listener) = network_with(vec![Faults::new().cut_after_sent(2)]);
    let server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        let _ = first.try_frame().await;
        // 监听方不在了：重连连不上。
        drop(listener);
    });
    let client = ExecClient::connect(connector, reconnecting(1000))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), client.call(stat(), Body::empty()))
        .await
        .unwrap();
    assert!(
        matches!(result, Err(ClientError::Disconnected(_))),
        "{result:?}"
    );
    assert!(matches!(
        client.call(stat(), Body::empty()).await,
        Err(ClientError::Disconnected(_))
    ));
    assert!(!client.is_connected());
    server.await.unwrap();
}

/// remote-exec.md §19.8：退避从 `delay` 起翻倍，到 `max_delay` 为止。
#[test]
fn the_backoff_doubles_up_to_the_cap() {
    let policy = Reconnect {
        attempts: 12,
        delay: Duration::from_millis(500),
        max_delay: Duration::from_secs(15),
    };
    let waits: Vec<u64> = (1..=8)
        .map(|attempt| u64::try_from(backoff(policy, attempt).as_millis()).unwrap())
        .collect();
    assert_eq!(waits, [500, 1000, 2000, 4000, 8000, 15000, 15000, 15000]);
    assert_eq!(backoff(policy, 100), Duration::from_secs(15));
}

/// §19.8：连接的状态按 连上 → 断开重连 → 连上 的顺序变化；关闭后是 Closed。
#[tokio::test]
async fn the_connection_state_follows_the_link() {
    let (connector, listener) = network_with(Vec::new());
    let switches = connector.clone();
    let (cut, cut_rx) = tokio::sync::oneshot::channel::<()>();
    let server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        let _ = cut_rx.await;
        switches.switch(0).cut();
        let mut second = Peer::accept(&listener).await;
        second.handshake("exec-1", Capabilities::ports()).await;
        let _ = second.try_frame().await;
    });
    let client = ExecClient::connect(connector, reconnecting(1000))
        .await
        .unwrap();
    let mut states = client.states();
    let mut seen = vec![states.borrow_and_update().clone()];
    cut.send(()).unwrap();
    while seen.len() < 4 {
        tokio::time::timeout(Duration::from_secs(5), states.changed())
            .await
            .unwrap()
            .unwrap();
        let state = states.borrow_and_update().clone();
        let again = matches!(state, ConnectionState::Connected { epoch: 2, .. });
        seen.push(state);
        if again {
            break;
        }
    }
    assert!(
        matches!(&seen[0], ConnectionState::Connected { epoch: 1, server_instance } if server_instance == "exec-1"),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .any(|state| matches!(state, ConnectionState::Reconnecting { .. })),
        "{seen:?}"
    );
    assert!(
        matches!(
            seen.last(),
            Some(ConnectionState::Connected { epoch: 2, .. })
        ),
        "{seen:?}"
    );
    client.close().await;
    assert!(matches!(
        &*client.states().borrow(),
        ConnectionState::Closed {
            error: ClientError::Closed
        }
    ));
    drop(server);
}

/// §19.8：执行器换了实例、账本相同（常驻进程重启）时，发出过的有副作用的调用照常补发（同一个调用 ID），由账本回结果。
#[tokio::test]
async fn after_a_restart_with_the_same_ledger_mutations_are_resent() {
    let (connector, listener) = network_with(Vec::new());
    let switches = connector.clone();
    let with_ledger = |instance: &str| {
        let mut welcome = welcome(instance, Capabilities::ports());
        welcome.ledger = Some("ledger-1".to_owned());
        welcome
    };
    let server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake_with(with_ledger("exec-1")).await;
        let (sent, _) = first.request().await;
        switches.switch(0).cut();
        let mut second = Peer::accept(&listener).await;
        second.handshake_with(with_ledger("exec-2")).await;
        let (again, _) = second.request().await;
        assert_eq!((again.call, again.request), (sent.call, put()));
        let header = rust_cli_exec_wire::envelope::ReplyHeader {
            call: again.call,
            result: ReplyResult::Err(rust_cli_exec_wire::envelope::WireError::Interrupted {
                message: "files.put_file did not finish before the executor restarted".to_owned(),
            }),
        };
        second
            .send(
                rust_cli_exec_wire::frame::Kind::Reply,
                &header,
                &Body::empty(),
            )
            .await;
    });
    let client = ExecClient::connect(connector, reconnecting(1000))
        .await
        .unwrap();
    let answer = tokio::time::timeout(Duration::from_secs(5), client.call(put(), Body::empty()))
        .await
        .unwrap()
        .unwrap();
    assert!(
        matches!(
            answer.error(),
            Some(rust_cli_exec_wire::envelope::WireError::Interrupted { .. })
        ),
        "{answer:?}"
    );
    server.await.unwrap();
}
