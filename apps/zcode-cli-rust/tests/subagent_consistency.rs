use anyhow::Result;
use async_trait::async_trait;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use zcode_cli_rust::{app::Engine, contract::*, domain::session::Session};

struct Store {
    stage: &'static str,
    intercepted: AtomicBool,
    gate: mpsc::UnboundedSender<oneshot::Sender<bool>>,
}
#[async_trait]
impl SessionStore for Store {
    async fn load_index(&self, _: &str) -> Result<BTreeMap<String, Value>> {
        Ok(BTreeMap::new())
    }
    async fn lookup_ack(&self, _: &str, _: &str) -> Result<Option<Value>> {
        Ok(None)
    }
    async fn load(&self, _: &str) -> Result<(Vec<Session>, BTreeMap<String, Value>)> {
        Ok((vec![], BTreeMap::new()))
    }
    async fn load_session(&self, _: &str, _: &str) -> Result<Option<Session>> {
        Ok(Some(Session::new(
            "parent".into(),
            "workspace".into(),
            "p".into(),
            "m".into(),
            "none".into(),
            "old".into(),
            1,
        )))
    }
    async fn commit(
        &self,
        _: &str,
        session: Option<&mut Session>,
        _: Option<(String, Value)>,
    ) -> Result<()> {
        let selected = session.as_ref().is_some_and(|s| match self.stage {
            "child" => s.agent_profile.is_some(),
            "parent" => s.children.values().any(|t| t.running()),
            _ => s.children.values().any(|t| t.status == "completed"),
        });
        if selected && !self.intercepted.swap(true, Ordering::SeqCst) {
            let (tx, rx) = oneshot::channel();
            self.gate.send(tx)?;
            anyhow::ensure!(rx.await?, "injected child commit failure");
        }
        Ok(())
    }
}
struct Model {
    calls: mpsc::UnboundedSender<String>,
}
#[async_trait]
impl ModelPort for Model {
    async fn complete(
        &self,
        messages: Vec<Value>,
        _: &[Value],
        _: &EventSink,
        _: &CancellationToken,
    ) -> std::result::Result<ModelOutput, ModelFailure> {
        let last = messages.last().unwrap();
        self.calls
            .send(last["content"].as_str().unwrap().into())
            .unwrap();
        let calls = if last["content"] == "delegate" {
            vec![
                json!({"id":"launch","type":"function","function":{"name":"Agent","arguments":"{\"description\":\"child\",\"prompt\":\"child-work\"}"}}),
            ]
        } else {
            vec![]
        };
        let mut message =
            json!({"role":"assistant","content":if calls.is_empty(){"child result"}else{""}});
        if !calls.is_empty() {
            message["tool_calls"] = json!(calls);
        }
        Ok(ModelOutput {
            message,
            calls,
            usage: json!({}),
            output_limit: false,
        })
    }
}
struct Tools;
#[async_trait]
impl ToolPort for Tools {
    fn definitions(&self) -> Vec<Value> {
        vec![
            json!({"type":"function","function":{"name":"Agent","description":"Agent","parameters":{"type":"object"}}}),
        ]
    }
    fn requires_permission(&self, _: &str) -> bool {
        false
    }
    async fn execute(&self, _: &str, _: &Value, _: &CancellationToken) -> Result<String> {
        panic!("Agent must execute through owner")
    }
    async fn agent_output(&self, _: &str, _: &str) -> Result<String> {
        Ok("/fixture/agent.output".into())
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
async fn child_start_and_result_require_both_owners_to_commit_and_failure_releases_waiters() {
    for stage in ["child", "parent", "result"] {
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
                    context: Arc::new(zcode_cli_host::context_source::WorkspaceContext::new(
                        std::env::temp_dir(),
                        std::env::temp_dir().join("fixture-empty-home"),
                        false,
                    )),
                    store: Arc::new(Store {
                        stage,
                        intercepted: AtomicBool::new(false),
                        gate,
                    }),
                    model: Some(Arc::new(Model { calls })),
                    tools: Arc::new(Tools),
                    clock: Arc::new(Clock(AtomicUsize::new(1))),
                },
            )
            .await
            .unwrap();
            let (tx, rx) = mpsc::channel(8);
            let (out, mut output) = mpsc::channel(32);
            let running = tokio::spawn(engine.serve(rx, out, CancellationToken::new()));
            let drain = tokio::spawn(async move { while output.recv().await.is_some() {} });
            tx.send(Input::Request(serde_json::from_value(json!({"id":1,"method":"v4/command","params":{"commandId":"parent-input","clientId":"test","sessionId":"parent","type":"sendText","issuedAt":1,"payload":{"text":"delegate"}}})).unwrap())).await.unwrap();
            let gate = tokio::time::timeout(std::time::Duration::from_secs(3), gates.recv())
                .await
                .unwrap()
                .unwrap();
            assert_eq!(requests.try_recv().unwrap(), "delegate");
            if stage == "result" {
                assert_eq!(requests.try_recv().unwrap(), "child-work");
            }
            assert!(requests.try_recv().is_err());
            gate.send(success).unwrap();
            if success {
                tokio::time::timeout(std::time::Duration::from_secs(3), requests.recv())
                    .await
                    .unwrap()
                    .unwrap();
                tx.send(Input::Eof).await.unwrap();
            }
            let result = tokio::time::timeout(std::time::Duration::from_secs(3), running)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result.is_ok(), success);
            if !success {
                assert!(requests.try_recv().is_err());
            }
            drain.await.unwrap();
        }
    }
}
