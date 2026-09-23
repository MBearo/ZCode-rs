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
use zcode_rust::{
    app::Engine,
    contract::*,
    domain::{goal::Goal, session::Session},
};

struct Store {
    gate: mpsc::UnboundedSender<oneshot::Sender<bool>>,
    intercepted: AtomicBool,
    verdict: bool,
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
            "session".into(),
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
        let matches = session
            .as_ref()
            .and_then(|s| s.goal.as_ref())
            .is_some_and(|goal| {
                if self.verdict {
                    !goal.verifications.is_empty()
                } else {
                    goal.status == "verifying"
                }
            });
        if matches && !self.intercepted.swap(true, Ordering::SeqCst) {
            let (tx, rx) = oneshot::channel();
            self.gate.send(tx)?;
            anyhow::ensure!(rx.await?, "injected goal storage failure");
        }
        Ok(())
    }
}
struct Model {
    calls: mpsc::UnboundedSender<bool>,
    verifications: AtomicUsize,
}
#[async_trait]
impl ModelPort for Model {
    async fn complete(
        &self,
        messages: Vec<Value>,
        tools: &[Value],
        _: &EventSink,
        _: &CancellationToken,
    ) -> std::result::Result<ModelOutput, ModelFailure> {
        let verify = messages.last().unwrap()["content"]
            .as_str()
            .unwrap()
            .starts_with("Verify whether");
        self.calls.send(verify).unwrap();
        let content = if verify {
            assert!(tools.is_empty());
            json!({"passed":self.verifications.fetch_add(1,Ordering::SeqCst)>0,"reason":"gap","nextAction":"check"}).to_string()
        } else {
            "work".into()
        };
        Ok(ModelOutput {
            output_limit: false,
            message: json!({"role":"assistant","content":content}),
            calls: vec![],
            usage: json!({"prompt_tokens":10,"completion_tokens":3}),
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
        panic!("no tool expected")
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
async fn goal_start_and_verdict_are_durable_barriers_and_failure_stops_the_next_request() {
    for verdict in [false, true] {
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
                        intercepted: AtomicBool::new(false),
                        verdict,
                    }),
                    model: Some(Arc::new(Model {
                        calls,
                        verifications: AtomicUsize::new(0),
                    })),
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
            tx.send(Input::Request(serde_json::from_value(json!({"id":1,"method":"v4/command","params":{"commandId":"goal","clientId":"test","sessionId":"session","type":"sendGoalCommand","issuedAt":1,"payload":{"text":"deliver"}}})).unwrap())).await.unwrap();
            let gate = tokio::time::timeout(std::time::Duration::from_secs(3), gates.recv())
                .await
                .unwrap()
                .unwrap();
            assert!(!requests.try_recv().unwrap());
            if verdict {
                assert!(requests.try_recv().unwrap());
            }
            assert!(requests.try_recv().is_err());
            gate.send(success).unwrap();
            if success {
                assert_eq!(
                    tokio::time::timeout(std::time::Duration::from_secs(3), requests.recv())
                        .await
                        .unwrap()
                        .unwrap(),
                    !verdict
                );
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
#[test]
fn goal_clock_budget_and_restart_preserve_confirmed_work_only() {
    let mut goal = Goal::new("goal".into(), "work".into(), 1000);
    goal.token_budget = Some(10);
    goal.account(&json!({"prompt_tokens":7,"completion_tokens":3}), 2500);
    assert!(goal.exhausted());
    let mut session = Session::new(
        "s".into(),
        "w".into(),
        "p".into(),
        "m".into(),
        "none".into(),
        "old".into(),
        1,
    );
    session.goal = Some(goal);
    session.recover("new".into(), 100000);
    let goal = session.goal.unwrap();
    assert_eq!(goal.status, "paused");
    assert_eq!(goal.time_used_ms, 1500);
    assert_eq!(goal.active_run_started_at_ms, None);
}
