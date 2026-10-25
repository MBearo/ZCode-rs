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
    // F96：这个调用在被切断的连接上可能发出去了（Disconnected），也可能还没发（NotSent，里面是同一个原因）。
    assert!(
        matches!(
            &result,
            Err(ClientError::Disconnected(_)) | Err(ClientError::NotSent(_))
        ),
        "{result:?}"
    );
    // 客户端不工作之后的调用不会发出去。
    assert!(matches!(
        client.call(stat(), Body::empty()).await,
        Err(ClientError::NotSent(inner)) if matches!(*inner, ClientError::Disconnected(_))
    ));
    assert!(!client.is_connected());
    server.await.unwrap();
}

/// remote-exec.md §19.8：退避从 `delay` 起翻倍，到 `max_delay` 为止。
#[test]
fn the_backoff_doubles_up_to_the_cap() {
    let policy = Reconnect {
        attempts: Some(12),
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
        // 连接留着直到客户端结束：握手后客户端马上发 ping（R4 量延迟，remote-exec.md §21.2），读到帧不算结束。
        while second.try_frame().await.is_some() {}
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

/// F96 的连接器（remote-exec.md §23.4）：包住内存网络，测试可以让之后的连接失败几次（可恢复）或被拒绝（不可恢复），
/// 并数连接了几次。
struct Flaky {
    inner: std::sync::Arc<crate::test_support::FaultyConnector>,
    fails: std::sync::atomic::AtomicU32,
    reject: std::sync::Mutex<Option<String>>,
    connects: std::sync::atomic::AtomicU32,
}

impl Flaky {
    fn new(inner: std::sync::Arc<crate::test_support::FaultyConnector>) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            inner,
            fails: std::sync::atomic::AtomicU32::new(0),
            reject: std::sync::Mutex::new(None),
            connects: std::sync::atomic::AtomicU32::new(0),
        })
    }

    fn connects(&self) -> u32 {
        self.connects.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl rust_cli_exec_transport::Connector for Flaky {
    async fn connect(
        &self,
    ) -> Result<Box<dyn rust_cli_exec_transport::Link>, rust_cli_exec_transport::LinkError> {
        use std::sync::atomic::Ordering;
        self.connects.fetch_add(1, Ordering::SeqCst);
        if let Some(message) = self.reject.lock().unwrap().clone() {
            return Err(rust_cli_exec_transport::LinkError::Rejected(message));
        }
        let failing = self
            .fails
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |left| {
                left.checked_sub(1)
            });
        if failing.is_ok() {
            // 像真的连接那样花一点时间（“正在连”的状态看得到）。
            tokio::time::sleep(Duration::from_millis(5)).await;
            return Err(rust_cli_exec_transport::LinkError::Connect(
                "the network is down".to_owned(),
            ));
        }
        self.inner.connect().await
    }

    fn describe(&self) -> String {
        "flaky".to_owned()
    }
}

fn endless(delay_ms: u64) -> crate::ClientOptions {
    let mut options = reconnecting(1000);
    options.reconnect = Reconnect {
        attempts: None,
        delay: Duration::from_millis(delay_ms),
        max_delay: Duration::from_millis(delay_ms),
    };
    options
}

/// F96（remote-exec.md §23.4）：次数不限时一直重连，超过 R2 的 12 次也不停；连上之后同一个客户端照常工作。
#[tokio::test]
async fn endless_reconnecting_keeps_trying_past_twelve_attempts() {
    let (connector, listener) = network_with(Vec::new());
    let switches = connector.clone();
    let flaky = Flaky::new(connector);
    let server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        let mut second = Peer::accept(&listener).await;
        second.handshake("exec-1", Capabilities::ports()).await;
        let (again, _) = second.request().await;
        second
            .reply(again.call, Reply::Stat(None), &Body::empty())
            .await;
        while second.try_frame().await.is_some() {}
        drop(first);
    });
    let client = ExecClient::connect(flaky.clone(), endless(1))
        .await
        .unwrap();
    flaky.fails.store(15, std::sync::atomic::Ordering::SeqCst);
    switches.switch(0).cut();
    let answer = tokio::time::timeout(Duration::from_secs(10), client.call(stat(), Body::empty()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(answer.result, ReplyResult::Ok(Reply::Stat(None)));
    // 第一次连接 + 15 次失败 + 连上的那一次。
    assert_eq!(flaky.connects(), 17);
    assert!(matches!(
        &*client.states().borrow(),
        ConnectionState::Connected { epoch: 2, .. }
    ));
    client.close().await;
    drop(server);
}

/// F96：重连中的状态带下一次的时刻与最近一次的原因；正在连的那一次没有下一次的时刻。
#[tokio::test]
async fn reconnecting_states_carry_the_next_retry_and_the_last_reason() {
    let (connector, listener) = network_with(Vec::new());
    let switches = connector.clone();
    let flaky = Flaky::new(connector);
    let _server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        while first.try_frame().await.is_some() {}
    });
    let client = ExecClient::connect(flaky.clone(), endless(40))
        .await
        .unwrap();
    let mut states = client.states();
    flaky
        .fails
        .store(u32::MAX, std::sync::atomic::Ordering::SeqCst);
    switches.switch(0).cut();
    let mut seen = Vec::new();
    while seen.len() < 6 {
        tokio::time::timeout(Duration::from_secs(5), states.changed())
            .await
            .unwrap()
            .unwrap();
        seen.push(states.borrow_and_update().clone());
    }
    assert!(
        seen.iter().any(|state| matches!(
            state,
            ConnectionState::Reconnecting {
                attempt: 0,
                retry_at: Some(_),
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|state| matches!(
            state,
            ConnectionState::Reconnecting { attempt, retry_at: None, .. } if *attempt >= 1
        )),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|state| matches!(
            state,
            ConnectionState::Reconnecting { attempt, retry_at: Some(_), reason } if *attempt >= 1 && reason.contains("the network is down")
        )),
        "{seen:?}"
    );
    client.close().await;
}

/// F96：重连被目标拒绝（ssh 认证失败、主机密钥不对、执行器程序不在了，由连接器标成 `Rejected`）时不再重试，马上关闭。
#[tokio::test]
async fn a_rejected_reconnect_closes_the_client_at_once() {
    let (connector, listener) = network_with(Vec::new());
    let switches = connector.clone();
    let flaky = Flaky::new(connector);
    let _server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        while first.try_frame().await.is_some() {}
    });
    let client = ExecClient::connect(flaky.clone(), endless(1))
        .await
        .unwrap();
    let mut states = client.states();
    *flaky.reject.lock().unwrap() = Some("Permission denied (publickey).".to_owned());
    switches.switch(0).cut();
    tokio::time::timeout(
        Duration::from_secs(5),
        states.wait_for(|state| matches!(state, ConnectionState::Closed { .. })),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        matches!(
            &*client.states().borrow(),
            ConnectionState::Closed { error: ClientError::Rejected(message) } if message.contains("Permission denied")
        ),
        "{:?}",
        client.states().borrow()
    );
    assert_eq!(flaky.connects(), 2, "one reconnect attempt only");
}

/// F96（remote-exec.md §23.6）：关闭时还没发出去的调用是 `NotSent`（没有执行），发出过的是 `Closed`（结果不明）。
#[tokio::test]
async fn closing_tells_unsent_calls_from_sent_ones() {
    let (connector, listener) = network_with(Vec::new());
    let switches = connector.clone();
    let flaky = Flaky::new(connector);
    let (sent, sent_rx) = tokio::sync::oneshot::channel::<()>();
    let _server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        let _ = first.request().await;
        let _ = sent.send(());
        while first.try_frame().await.is_some() {}
    });
    let client = ExecClient::connect(flaky.clone(), endless(5))
        .await
        .unwrap();
    let written = {
        let client = client.clone();
        rust_cli_base::owned(async move { client.call(put(), Body::empty()).await })
    };
    sent_rx.await.unwrap();
    // 执行器那头收到了第一个调用；之后断开、再也连不上。
    flaky
        .fails
        .store(u32::MAX, std::sync::atomic::Ordering::SeqCst);
    switches.switch(0).cut();
    let mut states = client.states();
    tokio::time::timeout(
        Duration::from_secs(5),
        states.wait_for(|state| matches!(state, ConnectionState::Reconnecting { .. })),
    )
    .await
    .unwrap()
    .unwrap();
    let unsent = {
        let client = client.clone();
        rust_cli_base::owned(async move { client.call(stat(), Body::empty()).await })
    };
    tokio::time::sleep(Duration::from_millis(20)).await;
    client.close().await;
    assert_eq!(written.await.unwrap(), Err(ClientError::Closed));
    assert!(
        matches!(unsent.await.unwrap(), Err(ClientError::NotSent(_))),
        "a call made while reconnecting was never sent"
    );
    assert!(matches!(
        client.call(stat(), Body::empty()).await,
        Err(ClientError::NotSent(_))
    ));
}

/// F98（remote-exec.md §25.4.1）：等退避的时候“立即重试”马上开始下一次，不等倒计时（这里是 60 s）；连着时不做事、回 false。
/// 旧代码没有这个口子，只能等到下一次的时刻。
#[tokio::test]
async fn retrying_now_skips_the_rest_of_the_backoff() {
    let (connector, listener) = network_with(Vec::new());
    let switches = connector.clone();
    let flaky = Flaky::new(connector);
    let _server = rust_cli_base::owned(async move {
        let mut first = Peer::accept(&listener).await;
        first.handshake("exec-1", Capabilities::ports()).await;
        let mut second = Peer::accept(&listener).await;
        second.handshake("exec-1", Capabilities::ports()).await;
        while second.try_frame().await.is_some() {}
        drop(first);
    });
    let client = ExecClient::connect(flaky.clone(), endless(60_000))
        .await
        .unwrap();
    assert!(!client.retry_now(), "nothing to retry while connected");
    let mut states = client.states();
    switches.switch(0).cut();
    tokio::time::timeout(
        Duration::from_secs(5),
        states.wait_for(|state| {
            matches!(
                state,
                ConnectionState::Reconnecting {
                    retry_at: Some(_),
                    ..
                }
            )
        }),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(client.retry_now());
    tokio::time::timeout(
        Duration::from_secs(5),
        states.wait_for(|state| matches!(state, ConnectionState::Connected { epoch: 2, .. })),
    )
    .await
    .expect("reconnected without waiting for the backoff")
    .unwrap();
    assert_eq!(flaky.connects(), 2);
    client.close().await;
}
