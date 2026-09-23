use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use zcode_rust::{
    app::Engine,
    contract::*,
    domain::{context::ContextPolicy, session::Session},
};
struct Store {
    gate: mpsc::UnboundedSender<oneshot::Sender<bool>>,
    commits: AtomicUsize,
    continuation: bool,
}
#[async_trait]
impl SessionStore for Store {
    async fn load_index(&self, _: &str) -> Result<BTreeMap<String, Value>> {
        Ok(BTreeMap::new())
    }
    async fn lookup_ack(&self, _: &str, _: &str) -> Result<Option<Value>> {
        Ok(None)
    }
    async fn load_session(&self, workspace: &str, id: &str) -> Result<Option<Session>> {
        Ok(self
            .load(workspace)
            .await?
            .0
            .into_iter()
            .find(|s| s.id == id))
    }
    async fn load(&self, _: &str) -> Result<(Vec<Session>, BTreeMap<String, Value>)> {
        let mut s = Session::new(
            "session".into(),
            "workspace".into(),
            "p".into(),
            "m".into(),
            "none".into(),
            "old".into(),
            1,
        );
        s.messages = vec![
            json!({"role":"user","content":"old".repeat(1000)}),
            json!({"role":"assistant","content":"previous"}),
        ];
        Ok((vec![s], BTreeMap::new()))
    }
    async fn commit(
        &self,
        _: &str,
        session: Option<&mut Session>,
        _: Option<(String, Value)>,
    ) -> Result<()> {
        if session.as_ref().is_some_and(|s| {
            if self.continuation {
                s.messages.iter().any(|m| m["content"] == "partial")
            } else {
                s.context.summary.is_some()
            }
        }) && self.commits.fetch_add(1, Ordering::SeqCst) == 0
        {
            let (tx, rx) = oneshot::channel();
            self.gate.send(tx)?;
            anyhow::ensure!(rx.await?, "injected context commit failure");
        }
        Ok(())
    }
}
struct Model {
    calls: mpsc::UnboundedSender<Vec<Value>>,
    continuation: bool,
    count: AtomicUsize,
}
#[async_trait]
impl ModelPort for Model {
    fn context_policy(&self) -> ContextPolicy {
        if self.continuation {
            return ContextPolicy::default();
        }
        ContextPolicy {
            // 完整默认 prompt 也消耗预算；旧历史应触发压缩，摘要后仍须容纳真实前缀。
            window: 3400,
            max_output: 100,
            buffer: 100,
            automatic: true,
        }
    }
    async fn complete(
        &self,
        messages: Vec<Value>,
        _: &[Value],
        _: &EventSink,
        _: &CancellationToken,
    ) -> std::result::Result<ModelOutput, ModelFailure> {
        self.calls.send(messages.to_vec()).unwrap();
        let limited = self.continuation && self.count.fetch_add(1, Ordering::SeqCst) == 0;
        Ok(ModelOutput {
            output_limit: limited,
            message: json!({"role":"assistant","content":if limited {"partial"} else {"summary"}}),
            calls: vec![],
            usage: json!({}),
        })
    }
}
struct Tools;
#[async_trait]
impl ToolPort for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![]
    }
    fn requires_permission(&self, _: &str) -> bool {
        false
    }
    async fn execute(&self, _: &str, _: &Value, _: &CancellationToken) -> Result<String> {
        panic!("no tools during compact")
    }
}
struct Clock(AtomicUsize);
impl RuntimeClock for Clock {
    fn id(&self) -> String {
        self.0.fetch_add(1, Ordering::SeqCst).to_string()
    }
    fn now(&self) -> u64 {
        1000
    }
}
#[tokio::test]
async fn summary_boundary_must_commit_before_next_request_and_failure_stops_execution() {
    check_commit_barrier(false).await;
}
#[tokio::test]
async fn partial_output_must_commit_before_continuation_and_failure_stops_execution() {
    check_commit_barrier(true).await;
}
async fn check_commit_barrier(continuation: bool) {
    for success in [false, true] {
        let (gate, mut gates) = mpsc::unbounded_channel();
        let (calls, mut requests) = mpsc::unbounded_channel();
        let engine = Engine::new(
            "workspace".into(),
            Some(ModelIdentity {
                provider_id: "p".into(),
                model_id: "m".into(),
                reasoning_level: "none".into(),
            }),
            RuntimePorts {
                context: Arc::new(zcode_rust::adapters::context_source::WorkspaceContext::new(
                    std::env::temp_dir(),
                    std::env::temp_dir().join("fixture-empty-home"),
                    false,
                )),
                store: Arc::new(Store {
                    gate,
                    commits: AtomicUsize::new(0),
                    continuation,
                }),
                model: Some(Arc::new(Model {
                    calls,
                    continuation,
                    count: AtomicUsize::new(0),
                })),
                tools: Arc::new(Tools),
                clock: Arc::new(Clock(AtomicUsize::new(1))),
            },
        )
        .await
        .unwrap();
        let (tx, rx) = mpsc::channel(10);
        let (out, mut output) = mpsc::channel(20);
        let running = tokio::spawn(engine.serve(rx, out, CancellationToken::new()));
        tx.send(Input::Request(serde_json::from_value(json!({"id":1,"method":"v4/command","params":{"commandId":"next","clientId":"test","sessionId":"session","type":"sendText","issuedAt":1,"payload":{"text":"continue"}}})).unwrap())).await.unwrap();
        let summary = tokio::time::timeout(std::time::Duration::from_secs(2), requests.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            summary[0]["content"]
                .as_str()
                .unwrap()
                .contains("Summarize"),
            !continuation
        );
        let gate = tokio::time::timeout(std::time::Duration::from_secs(2), gates.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(requests.try_recv().is_err());
        gate.send(success).unwrap();
        if success {
            let next = tokio::time::timeout(std::time::Duration::from_secs(2), requests.recv())
                .await
                .unwrap()
                .unwrap();
            let next = serde_json::to_string(&next).unwrap();
            if continuation {
                assert!(next.contains("partial"));
                assert!(next.contains("Output token limit hit. Resume directly"));
            } else {
                assert!(!next.contains(&"old".repeat(1000)));
                assert!(next.contains("summary"));
            }
            while let Some(batch) = output.recv().await {
                if batch.iter().any(|m| m["id"] == 1) {
                    break;
                }
            }
            tx.send(Input::Eof).await.unwrap();
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), running)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result.is_ok(), success);
        assert!(requests.try_recv().is_err());
    }
}
