use std::sync::Arc;
use std::time::Duration;

use rust_cli_base::TaskScope;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, duplex};

use super::*;

/// 回声服务：`echo` 回参数；`then` 回复之后再发一个通知；`ask` 向对端发请求、把对端的回复当结果；其余 -32601。
struct Echo {
    peer: Peer,
    seen: Mutex<Vec<String>>,
}

#[async_trait]
impl Service for Echo {
    async fn request(&self, method: &str, params: Value) -> Result<Reply, RpcError> {
        match method {
            "echo" => Ok(Reply::new(params)),
            "then" => {
                let peer = self.peer.clone();
                Ok(Reply::then(
                    json!({"ack": true}),
                    Box::new(move || {
                        Box::pin(async move {
                            peer.notify("after", &json!({"first": true})).await;
                        })
                    }),
                ))
            }
            "ask" => self
                .peer
                .request("host/answer", &params)
                .await
                .map(Reply::new),
            other => Err(RpcError::method_not_found(other)),
        }
    }

    async fn notification(&self, method: &str, _params: Value) {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(method.to_owned());
    }
}

struct Harness {
    to_server: tokio::io::DuplexStream,
    from_server: tokio::io::Lines<BufReader<tokio::io::DuplexStream>>,
    service: Arc<Echo>,
    _scope: TaskScope,
}

impl Harness {
    fn new(options: Options) -> Self {
        let scope = TaskScope::root("test", |_| {});
        let (to_server, server_in) = duplex(1 << 20);
        let (server_out, from_server) = duplex(1 << 20);
        let (peer, _writer) = start(server_out, &scope.spawner(), options);
        let service = Arc::new(Echo {
            peer: peer.clone(),
            seen: Mutex::new(Vec::new()),
        });
        let spawner = scope.spawner();
        drop(scope.spawner().spawn(
            "serve",
            serve(
                BufReader::new(server_in),
                peer,
                service.clone(),
                spawner,
                options,
            ),
        ));
        Self {
            to_server,
            from_server: BufReader::new(from_server).lines(),
            service,
            _scope: scope,
        }
    }

    async fn send(&mut self, line: &str) {
        self.to_server
            .write_all(format!("{line}\n").as_bytes())
            .await
            .unwrap();
    }

    async fn next(&mut self) -> Value {
        let line = tokio::time::timeout(Duration::from_secs(5), self.from_server.next_line())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        serde_json::from_str(&line).unwrap()
    }
}

#[tokio::test]
async fn requests_are_answered_with_the_same_id_and_unknown_methods_fail() {
    let mut harness = Harness::new(Options::default());
    harness
        .send(r#"{"id":1,"method":"echo","params":{"a":1}}"#)
        .await;
    assert_eq!(harness.next().await, json!({"id": 1, "result": {"a": 1}}));
    harness.send(r#"{"id":"x","method":"nope"}"#).await;
    assert_eq!(
        harness.next().await,
        json!({"id": "x", "error": {"code": -32601, "message": "Method not found: nope"}})
    );
    // 认不出 id 的坏行不回复；认得出的回 -32600。
    harness.send("garbage").await;
    harness.send(r#"{"id":2}"#).await;
    assert_eq!(harness.next().await["error"]["code"], json!(-32600));
}

#[tokio::test]
async fn the_after_reply_action_is_written_after_the_reply() {
    let mut harness = Harness::new(Options::default());
    harness.send(r#"{"id":5,"method":"then"}"#).await;
    assert_eq!(
        harness.next().await,
        json!({"id": 5, "result": {"ack": true}})
    );
    assert_eq!(
        harness.next().await,
        json!({"method": "after", "params": {"first": true}})
    );
}

#[tokio::test]
async fn outbound_requests_use_their_own_ids_and_get_the_peer_answer() {
    let mut harness = Harness::new(Options::default());
    harness
        .send(r#"{"id":9,"method":"ask","params":{"q":1}}"#)
        .await;
    let asked = harness.next().await;
    assert_eq!(asked["method"], json!("host/answer"));
    assert_eq!(asked["id"], json!("rs-1"));
    harness
        .send(r#"{"id":"rs-1","result":{"answer":42}}"#)
        .await;
    assert_eq!(
        harness.next().await,
        json!({"id": 9, "result": {"answer": 42}})
    );
    harness.send(r#"{"method":"host/note"}"#).await;
    harness
        .send(r#"{"id":10,"method":"echo","params":1}"#)
        .await;
    harness.next().await;
    assert_eq!(
        *harness
            .service
            .seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner),
        ["host/note"]
    );
}

#[tokio::test]
async fn closing_the_input_fails_the_waiting_outbound_requests() {
    let mut harness = Harness::new(Options::default());
    let peer = harness.service.peer.clone();
    let waiting = tokio::spawn(async move { peer.request("host/slow", &json!({})).await });
    assert_eq!(harness.next().await["method"], json!("host/slow"));
    harness.to_server.shutdown().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result.unwrap_err().code, CONNECTION_CLOSED);
    assert!(harness.service.peer.is_closed());
}

#[tokio::test]
async fn lines_over_the_limit_are_dropped_and_reading_continues() {
    let mut harness = Harness::new(Options {
        max_line_bytes: 64,
        queue: 8,
    });
    let long = format!(
        r#"{{"id":1,"method":"echo","params":"{}"}}"#,
        "x".repeat(200)
    );
    harness.send(&long).await;
    harness
        .send(r#"{"id":2,"method":"echo","params":"ok"}"#)
        .await;
    assert_eq!(harness.next().await, json!({"id": 2, "result": "ok"}));
}
