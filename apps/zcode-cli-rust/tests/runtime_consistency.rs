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
use zcode_cli_rust::{
    app::Engine,
    contract::*,
    domain::{protocol::Request, session::Session},
};

struct Commit {
    session: Option<Session>,
    queue: Vec<Value>,
    messages: Vec<Value>,
    phase: String,
    last_role: String,
    session_id: String,
    permission: Option<Value>,
    permit: oneshot::Sender<bool>,
}
struct Store {
    calls: AtomicUsize,
    tx: mpsc::UnboundedSender<Commit>,
}
#[async_trait]
impl SessionStore for Store {
    async fn load_index(&self, _: &str) -> Result<BTreeMap<String, Value>> {
        Ok(BTreeMap::new())
    }
    async fn lookup_ack(&self, _: &str, _: &str) -> Result<Option<Value>> {
        Ok(None)
    }
    async fn snapshot_attachment(
        &self,
        _: &str,
        mime: &str,
    ) -> Result<zcode_cli_domain::session::StoredAttachment> {
        Ok(zcode_cli_domain::session::StoredAttachment {
            path: "/fixture-snapshot".into(),
            source_path: Some("local.txt".into()),
            media_type: mime.into(),
            total_bytes: 4,
        })
    }
    async fn load(&self, _: &str) -> Result<(Vec<Session>, BTreeMap<String, Value>)> {
        Ok((vec![], BTreeMap::new()))
    }
    async fn commit(
        &self,
        _: &str,
        session: Option<&mut Session>,
        _: Option<(String, Value)>,
    ) -> Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let (permit, done) = oneshot::channel();
        self.tx.send(Commit {
            session: session.as_deref().cloned(),
            queue: session
                .as_ref()
                .map(|s| s.queue.clone())
                .unwrap_or_default(),
            messages: session
                .as_ref()
                .map(|s| s.messages.clone())
                .unwrap_or_default(),
            session_id: session.as_ref().map(|s| s.id.clone()).unwrap_or_default(),
            permission: session.as_ref().and_then(|s| s.pending.first()).cloned(),
            phase: session
                .as_ref()
                .map(|s| s.phase.clone())
                .unwrap_or_default(),
            last_role: session
                .as_ref()
                .and_then(|s| s.messages.last())
                .and_then(|v| v["role"].as_str())
                .unwrap_or("")
                .into(),
            permit,
        })?;
        anyhow::ensure!(done.await?, "injected commit failure");
        Ok(())
    }
}
struct Clock(AtomicUsize);
impl RuntimeClock for Clock {
    fn id(&self) -> String {
        format!("id{}", self.0.fetch_add(1, Ordering::SeqCst))
    }
    fn now(&self) -> u64 {
        1000
    }
}
struct Model {
    calls: AtomicUsize,
    tools: Vec<Value>,
    requests: mpsc::UnboundedSender<Vec<Value>>,
}
#[async_trait]
impl ModelPort for Model {
    async fn complete(
        &self,
        messages: Vec<Value>,
        _: &[Value],
        sink: &EventSink,
        _: &CancellationToken,
    ) -> std::result::Result<ModelOutput, ModelFailure> {
        self.requests.send(messages.to_vec()).unwrap();
        let first = self.calls.fetch_add(1, Ordering::SeqCst) == 0;
        // 来自旧 generation 的文本和结束事件不能污染当前 turn。
        sink.tx
            .send(RunEvent {
                session_id: sink.session_id.clone(),
                run_id: "stale".into(),
                event: Event::Text {
                    response_id: "stale".into(),
                    text: "stale pollution".into(),
                    reasoning: false,
                },
            })
            .await
            .unwrap();
        sink.tx
            .send(RunEvent {
                session_id: sink.session_id.clone(),
                run_id: "stale".into(),
                event: Event::Finished {
                    error: Some("stale error".into()),
                    model_failure: None,
                    cancelled: false,
                },
            })
            .await
            .unwrap();
        sink.tx
            .send(RunEvent {
                session_id: sink.session_id.clone(),
                run_id: "stale".into(),
                event: Event::ToolCleanupFailed("stale cleanup failure".into()),
            })
            .await
            .unwrap();
        let (reply, _) = oneshot::channel();
        sink.tx
            .send(RunEvent {
                session_id: sink.session_id.clone(),
                run_id: "stale".into(),
                event: Event::Todos {
                    call_id: "stale".into(),
                    write: Some(
                        serde_json::from_value(
                            json!([{"content":"stale todo","status":"pending","priority":"low"}]),
                        )
                        .unwrap(),
                    ),
                    reply,
                },
            })
            .await
            .unwrap();
        let calls = if first { self.tools.clone() } else { vec![] };
        let mut message = json!({"role":"assistant","content":if first{""}else{"done"}});
        if first {
            message["tool_calls"] = json!(calls);
        }
        Ok(ModelOutput {
            output_limit: false,
            message,
            calls,
            usage: json!({}),
        })
    }
}
struct ToolStart {
    n: usize,
    name: String,
    done: oneshot::Sender<()>,
}
struct Tools {
    cancellations: AtomicUsize,
    calls: AtomicUsize,
    gates: Option<mpsc::UnboundedSender<ToolStart>>,
}
#[async_trait]
impl ToolPort for Tools {
    async fn cancel_session(&self, _: &str, _: Option<&str>) -> Result<()> {
        self.cancellations.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
    fn definitions(&self) -> Vec<Value> {
        vec![]
    }
    fn requires_permission(&self, name: &str) -> bool {
        name == "GuardedWrite"
    }
    fn concurrent_safe(&self, name: &str) -> bool {
        name == "Read"
    }
    async fn execute(
        &self,
        name: &str,
        args: &Value,
        cancel: &CancellationToken,
    ) -> Result<String> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let n = args["n"].as_u64().unwrap() as usize;
        if let Some(gates) = &self.gates {
            let (done, rx) = oneshot::channel();
            gates.send(ToolStart {
                n,
                name: name.into(),
                done,
            })?;
            if name == "SlowCancel" {
                // 模拟子进程收到取消后仍需完成退出清理，Finished 不能越过这段清理。
                cancel.cancelled().await;
                rx.await?;
                anyhow::bail!("Cancelled after cleanup");
            }
            tokio::select! { _=cancel.cancelled()=>anyhow::bail!("Cancelled"), result=rx=>result? }
        }
        if name == "CleanupFailure" {
            return Err(zcode_cli_core_api::ProcessCleanupFailure.into());
        }
        if n == 1 {
            anyhow::bail!("injected tool failure");
        }
        Ok(format!("result {n}"))
    }
}
fn call(n: usize, name: &str) -> Value {
    json!({"id":format!("call{n}"),"function":{"name":name,"arguments":json!({"n":n}).to_string()}})
}
struct Runtime {
    input: mpsc::Sender<Input>,
    output: mpsc::Receiver<Vec<Value>>,
    running: tokio::task::JoinHandle<Result<()>>,
    store: Arc<Store>,
    model: Arc<Model>,
    tools: Arc<Tools>,
    commits: mpsc::UnboundedReceiver<Commit>,
    requests: mpsc::UnboundedReceiver<Vec<Value>>,
}
async fn start(calls: Vec<Value>, gates: Option<mpsc::UnboundedSender<ToolStart>>) -> Runtime {
    start_with_input(calls, gates, json!({"text":"run"})).await
}
async fn start_with_input(
    calls: Vec<Value>,
    gates: Option<mpsc::UnboundedSender<ToolStart>>,
    first: Value,
) -> Runtime {
    start_timed(calls, gates, first, (60_000, 300_000)).await
}
async fn start_timed(
    calls: Vec<Value>,
    gates: Option<mpsc::UnboundedSender<ToolStart>>,
    first: Value,
    timing: (u64, u64),
) -> Runtime {
    let (tx, commits) = mpsc::unbounded_channel();
    let store = Arc::new(Store {
        calls: AtomicUsize::new(0),
        tx,
    });
    let (requests_tx, requests) = mpsc::unbounded_channel();
    let model = Arc::new(Model {
        calls: AtomicUsize::new(0),
        tools: calls,
        requests: requests_tx,
    });
    let tools = Arc::new(Tools {
        cancellations: AtomicUsize::new(0),
        calls: AtomicUsize::new(0),
        gates,
    });
    let ports = RuntimePorts {
        context: Arc::new(zcode_cli_host::context_source::WorkspaceContext::new(
            std::env::temp_dir(),
            std::env::temp_dir().join("fixture-empty-home"),
            false,
        )),
        store: store.clone(),
        model: Some(model.clone()),
        tools: tools.clone(),
        clock: Arc::new(Clock(AtomicUsize::new(0))),
    };
    let engine = Engine::new(
        "workspace".into(),
        Some(ModelIdentity {
            provider_id: "p".into(),
            model_id: "m".into(),
            reasoning_level: "none".into(),
        }),
        ports,
    )
    .await
    .unwrap()
    .with_question_timing(timing.0, timing.1);
    let (input, rx) = mpsc::channel(32);
    let (out, output) = mpsc::channel(64);
    let running = tokio::spawn(engine.serve(rx, out, CancellationToken::new()));
    let request:Request=serde_json::from_value(json!({"id":1,"method":"v4/command","params":{"commandId":"first","clientId":"test","sessionId":null,"type":"createSession","issuedAt":1000,"payload":{"workspaceId":"workspace","firstInput":first}}})).unwrap();
    input.send(Input::Request(request)).await.unwrap();
    Runtime {
        input,
        output,
        running,
        store,
        model,
        tools,
        commits,
        requests,
    }
}
async fn receive<T>(rx: &mut mpsc::UnboundedReceiver<T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
        .await
        .unwrap()
        .unwrap()
}

#[tokio::test]
async fn question_registration_answer_timer_and_result_are_durable_barriers() {
    let question = json!({"id":"question-call","function":{"name":"AskUserQuestion","arguments":json!({"questions":[{"question":"Which?","header":"Choice","options":[{"label":"A","description":"First"},{"label":"B","description":"Second"}]}]}).to_string()}});
    for stage in [
        "registration",
        "answer",
        "snooze",
        "automatic",
        "result",
        "recover",
    ] {
        let timing = if stage == "automatic" {
            (0, 0)
        } else {
            (60_000, 300_000)
        };
        let mut runtime =
            start_timed(vec![question.clone()], None, json!({"text":"run"}), timing).await;
        let pending = loop {
            let c = receive(&mut runtime.commits).await;
            if c.permission.is_some() {
                break c;
            }
            c.permit.send(true).unwrap();
        };
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        let id = pending.session_id.clone();
        let interaction = pending.permission.as_ref().unwrap()["interactionId"].clone();
        pending.permit.send(stage != "registration").unwrap();
        if stage != "registration" {
            if stage != "automatic" {
                let request=serde_json::from_value(json!({"id":2,"method":"v4/command","params":{
                    "commandId":"answer","clientId":"test","sessionId":id,"issuedAt":1000,
                    "type":if stage=="snooze"{"snoozeInteractionAutoResolution"}else{"resolveInteraction"},
                    "payload":{"interactionId":interaction,"answer":{"action":"accept","content":{"answers":{"Which?":"A"}}}}
                }})).unwrap();
                runtime.input.send(Input::Request(request)).await.unwrap();
            }
            let c = receive(&mut runtime.commits).await;
            assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
            assert_eq!(c.last_role, "assistant");
            if stage == "recover" {
                let mut recovered = c.session.unwrap();
                c.permit.send(true).unwrap();
                // 模拟答案事务刚提交时崩溃：磁盘上没有 canonical tool，但答案必须可恢复。
                runtime.running.abort();
                let _ = runtime.running.await;
                recovered.recover("new-epoch".into(), 2000);
                assert!(recovered.pending.is_empty());
                assert_eq!(recovered.phase, "completedInterrupted");
                assert_eq!(
                    recovered.messages.last().unwrap()["tool_call_id"],
                    "question-call"
                );
                assert!(
                    recovered.messages.last().unwrap()["content"]
                        .as_str()
                        .unwrap()
                        .contains("\"Which?\"=\"A\"")
                );
                continue;
            }
            c.permit.send(stage == "result").unwrap();
            if stage == "result" {
                let c = receive(&mut runtime.commits).await;
                assert_eq!(c.last_role, "tool");
                assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
                c.permit.send(false).unwrap();
            }
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), runtime.running)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err(), "stage {stage}");
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.tools.calls.load(Ordering::SeqCst), 0);
        assert!(
            runtime.commits.try_recv().is_err(),
            "failed question transaction was resurrected"
        );
    }
}

#[tokio::test]
async fn attachment_input_cannot_execute_before_its_session_commit() {
    let mut runtime = start_with_input(vec![], None, json!({"text":"", "attachments":[{"ref":"local.txt","fileName":"local.txt","mime":"text/plain","bytes":4}]})).await;
    let commit = receive(&mut runtime.commits).await;
    assert_eq!(commit.last_role, "user");
    assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 0);
    assert!(runtime.requests.try_recv().is_err());
    commit.permit.send(false).unwrap();
    assert!(
        tokio::time::timeout(std::time::Duration::from_secs(3), runtime.running)
            .await
            .unwrap()
            .unwrap()
            .is_err()
    );
    assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn commit_failures_do_not_execute_or_resurrect_uncommitted_facts() {
    for fail_at in [1, 2, 3, 5] {
        let mut runtime = start(vec![call(0, "Read")], None).await;
        for ordinal in 1..=fail_at {
            let commit = receive(&mut runtime.commits).await;
            if ordinal <= 2 {
                assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 0);
            }
            if ordinal == 3 {
                assert_eq!(commit.last_role, "assistant");
                tokio::task::yield_now().await;
                assert_eq!(runtime.tools.calls.load(Ordering::SeqCst), 0);
            }
            if ordinal == 5 {
                assert_eq!(commit.last_role, "tool");
                tokio::task::yield_now().await;
                assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
            }
            commit.permit.send(ordinal != fail_at).unwrap();
        }
        let result = tokio::time::timeout(std::time::Duration::from_secs(3), runtime.running)
            .await
            .unwrap()
            .unwrap();
        assert!(result.is_err());
        assert_eq!(runtime.store.calls.load(Ordering::SeqCst), fail_at);
        assert_eq!(
            runtime.tools.calls.load(Ordering::SeqCst),
            usize::from(fail_at == 5)
        );
    }
}

#[tokio::test]
async fn four_readers_preserve_result_order_and_write_barrier() {
    let (tx, mut started) = mpsc::unbounded_channel();
    let calls = (0..8)
        .map(|n| call(n, if n == 6 { "Write" } else { "Read" }))
        .collect();
    let mut runtime = start(calls, Some(tx)).await;
    let (complete, finished) = oneshot::channel();
    let mut commits = runtime.commits;
    let database = tokio::spawn(async move {
        while let Some(commit) = commits.recv().await {
            let done = commit.phase == "completedSuccess";
            commit.permit.send(true).unwrap();
            if done {
                let _ = complete.send(());
                break;
            }
        }
    });
    let mut first = vec![];
    for n in 0..4 {
        let tool = receive(&mut started).await;
        assert_eq!(tool.n, n);
        first.push(tool);
    }
    assert!(started.try_recv().is_err());
    // 后面的 Read 先完成，首个仍在等待；不能越过写屏障或改变 canonical 结果顺序。
    for tool in first.drain(1..).rev() {
        tool.done.send(()).unwrap();
    }
    tokio::task::yield_now().await;
    assert!(started.try_recv().is_err());
    first.remove(0).done.send(()).unwrap();
    let a = receive(&mut started).await;
    let b = receive(&mut started).await;
    assert_eq!((a.n, b.n), (4, 5));
    b.done.send(()).unwrap();
    a.done.send(()).unwrap();
    let write = receive(&mut started).await;
    assert_eq!((write.n, write.name.as_str()), (6, "Write"));
    assert!(started.try_recv().is_err());
    write.done.send(()).unwrap();
    let last = receive(&mut started).await;
    assert_eq!(last.n, 7);
    last.done.send(()).unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), finished)
        .await
        .unwrap()
        .unwrap();
    receive(&mut runtime.requests).await;
    let request = receive(&mut runtime.requests).await;
    let results = request
        .iter()
        .filter(|m| m["role"] == "tool")
        .collect::<Vec<_>>();
    assert_eq!(results.len(), 8);
    for (n, result) in results.iter().enumerate() {
        assert_eq!(result["tool_call_id"], format!("call{n}"));
    }
    assert!(
        results[1]["content"]
            .as_str()
            .unwrap()
            .contains("injected tool failure")
    );
    let mut batch = runtime.output.recv().await.unwrap();
    let session = batch.remove(0)["result"]["result"]["sessionId"]
        .as_str()
        .unwrap()
        .to_owned();
    let request=serde_json::from_value(json!({"id":2,"method":"v4/conversation/rowsRange","params":{"sessionId":session,"limit":200}})).unwrap();
    runtime.input.send(Input::Request(request)).await.unwrap();
    let rows = runtime.output.recv().await.unwrap();
    assert!(!rows[0].to_string().contains("stale pollution"));
    runtime.input.send(Input::Eof).await.unwrap();
    runtime.running.await.unwrap().unwrap();
    database.await.unwrap();
}

#[tokio::test]
async fn permission_resolution_is_one_durable_commit_before_effects() {
    for allow_commit in [false, true] {
        let mut runtime = start(vec![call(0, "GuardedWrite")], None).await;
        for _ in 0..4 {
            receive(&mut runtime.commits)
                .await
                .permit
                .send(true)
                .unwrap();
        }
        let pending = receive(&mut runtime.commits).await;
        let interaction = pending.permission.as_ref().unwrap()["interactionId"].clone();
        pending.permit.send(true).unwrap();
        let request=serde_json::from_value(json!({"id":2,"method":"v4/command","params":{
            "commandId":"approve","clientId":"test","sessionId":pending.session_id,"type":"resolveInteraction","issuedAt":1000,
            "payload":{"interactionId":interaction,"answer":{"optionId":"allowOnce"}}
        }})).unwrap();
        runtime.input.send(Input::Request(request)).await.unwrap();
        let resolving = receive(&mut runtime.commits).await;
        assert!(resolving.permission.is_none());
        assert_eq!(runtime.tools.calls.load(Ordering::SeqCst), 0);
        resolving.permit.send(allow_commit).unwrap();
        if allow_commit {
            let result = receive(&mut runtime.commits).await;
            assert_eq!(result.last_role, "tool"); // No second permission write between approval and result.
            assert_eq!(runtime.tools.calls.load(Ordering::SeqCst), 1);
            result.permit.send(true).unwrap();
            loop {
                let commit = receive(&mut runtime.commits).await;
                let done = commit.phase == "completedSuccess";
                commit.permit.send(true).unwrap();
                if done {
                    break;
                }
            }
            runtime.input.send(Input::Eof).await.unwrap();
            runtime.running.await.unwrap().unwrap();
        } else {
            assert!(runtime.running.await.unwrap().is_err());
            assert_eq!(runtime.store.calls.load(Ordering::SeqCst), 6);
            assert_eq!(runtime.tools.calls.load(Ordering::SeqCst), 0);
        }
    }
}

async fn busy_fixture() -> (Runtime, String, ToolStart) {
    busy_fixture_with("Read").await
}
async fn busy_fixture_with(name: &str) -> (Runtime, String, ToolStart) {
    let (tx, mut tools) = mpsc::unbounded_channel();
    let mut runtime = start(vec![call(0, name)], Some(tx)).await;
    let first = receive(&mut runtime.commits).await;
    let id = first.session_id.clone();
    first.permit.send(true).unwrap();
    for _ in 0..3 {
        receive(&mut runtime.commits)
            .await
            .permit
            .send(true)
            .unwrap();
    }
    let tool = receive(&mut tools).await;
    (runtime, id, tool)
}
async fn send_busy(runtime: &Runtime, id: &str, delivery: &str) {
    let request = serde_json::from_value(json!({"id":2,"method":"v4/command","params":{
        "commandId":"busy", "clientId":"test", "sessionId":id, "type":"sendText", "issuedAt":1000,
        "payload":{"text":"new input", "requestedDelivery":delivery}
    }}))
    .unwrap();
    runtime.input.send(Input::Request(request)).await.unwrap();
}
async fn finish_success(mut runtime: Runtime) {
    loop {
        let commit = receive(&mut runtime.commits).await;
        let done = commit.phase == "completedSuccess";
        commit.permit.send(true).unwrap();
        if done {
            break;
        }
    }
    runtime.input.send(Input::Eof).await.unwrap();
    runtime.running.await.unwrap().unwrap();
}

#[tokio::test]
async fn guide_admission_and_consumption_are_durable_barriers() {
    for fail_at in ["admission", "consume", "none"] {
        let (mut runtime, id, tool) = busy_fixture().await;
        send_busy(&runtime, &id, "guide").await;
        let admission = receive(&mut runtime.commits).await;
        assert_eq!(admission.queue[0]["delivery"]["admitted"], "guide");
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        assert!(!tool.done.is_closed());
        admission.permit.send(fail_at != "admission").unwrap();
        if fail_at == "admission" {
            assert!(runtime.running.await.unwrap().is_err());
            assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
            continue;
        }
        tool.done.send(()).unwrap();
        let tool_commit = receive(&mut runtime.commits).await;
        assert_eq!(tool_commit.last_role, "tool");
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        tool_commit.permit.send(true).unwrap();
        let guide = receive(&mut runtime.commits).await;
        assert_eq!(guide.last_role, "user");
        assert!(
            guide.messages.last().unwrap()["content"]
                .as_str()
                .unwrap()
                .contains("new input")
        );
        assert!(guide.queue.is_empty());
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        guide.permit.send(fail_at != "consume").unwrap();
        if fail_at == "consume" {
            assert!(runtime.running.await.unwrap().is_err());
            assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        } else {
            receive(&mut runtime.requests).await;
            let next = receive(&mut runtime.requests).await;
            assert_eq!(next[next.len() - 2]["role"], "tool");
            assert_eq!(next.last().unwrap()["role"], "user");
            finish_success(runtime).await;
        }
    }
}

#[tokio::test]
async fn start_now_waits_for_reservation_terminal_and_promotion_commits() {
    for fail_at in ["admission", "terminal", "promotion", "none"] {
        let (mut runtime, id, tool) = busy_fixture().await;
        send_busy(&runtime, &id, "startNow").await;
        let admission = receive(&mut runtime.commits).await;
        assert_eq!(admission.queue[0]["delivery"]["admitted"], "startNow");
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        assert!(!tool.done.is_closed());
        admission.permit.send(fail_at != "admission").unwrap();
        if fail_at == "admission" {
            assert!(runtime.running.await.unwrap().is_err());
            assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
            continue;
        }
        let terminal = receive(&mut runtime.commits).await;
        assert_eq!(terminal.phase, "completedInterrupted");
        assert!(tool.done.is_closed());
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        terminal.permit.send(fail_at != "terminal").unwrap();
        if fail_at == "terminal" {
            assert!(runtime.running.await.unwrap().is_err());
            assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
            continue;
        }
        let promotion = receive(&mut runtime.commits).await;
        assert_eq!(promotion.phase, "running");
        assert_eq!(promotion.last_role, "user");
        assert!(promotion.queue.is_empty());
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        promotion.permit.send(fail_at != "promotion").unwrap();
        if fail_at == "promotion" {
            assert!(runtime.running.await.unwrap().is_err());
            assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        } else {
            receive(&mut runtime.requests).await;
            let next = receive(&mut runtime.requests).await;
            assert!(
                next.last().unwrap()["content"]
                    .as_str()
                    .unwrap()
                    .contains("new input")
            );
            assert_eq!(next[next.len() - 2]["role"], "tool");
            finish_success(runtime).await;
        }
    }
}

#[tokio::test]
async fn stop_during_start_now_reservation_holds_input_and_rejects_competing_promotion() {
    let (mut runtime, id, tool) = busy_fixture_with("SlowCancel").await;
    send_busy(&runtime, &id, "startNow").await;
    receive(&mut runtime.commits)
        .await
        .permit
        .send(true)
        .unwrap();
    // 消费 create 与 sendText 的 ACK；旧工具仍在清理，预留必须保持唯一。
    runtime.output.recv().await.unwrap();
    runtime.output.recv().await.unwrap();
    let command = |name: &str, kind: &str, payload: Value| {
        Input::Request(serde_json::from_value(json!({"id":name,"method":"v4/command","params":{
            "commandId":name,"clientId":"test","sessionId":id,"type":kind,"payload":payload,"issuedAt":1000
        }})).unwrap())
    };
    runtime
        .input
        .send(command(
            "other",
            "sendText",
            json!({"text":"competing", "requestedDelivery":"startNow"}),
        ))
        .await
        .unwrap();
    let rejection = runtime.output.recv().await.unwrap();
    assert_eq!(
        rejection[0]["result"]["reasonCode"],
        "guard.queuePromotionBusy"
    );
    runtime
        .input
        .send(command("stop", "stop", json!({})))
        .await
        .unwrap();
    let stop = receive(&mut runtime.commits).await;
    assert_eq!(stop.queue[0]["delivery"]["admitted"], "queue");
    assert_eq!(stop.queue[0]["dispatch"]["state"], "queued");
    stop.permit.send(true).unwrap();
    tool.done.send(()).unwrap();
    let terminal = receive(&mut runtime.commits).await;
    assert_eq!(terminal.phase, "completedInterrupted");
    assert_eq!(terminal.queue.len(), 1);
    terminal.permit.send(true).unwrap();
    runtime.input.send(Input::Eof).await.unwrap();
    runtime.running.await.unwrap().unwrap();
    assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn eof_cannot_promote_an_accepted_start_now_reservation() {
    let (mut runtime, id, tool) = busy_fixture_with("SlowCancel").await;
    send_busy(&runtime, &id, "startNow").await;
    receive(&mut runtime.commits)
        .await
        .permit
        .send(true)
        .unwrap();
    runtime.output.recv().await.unwrap();
    runtime.output.recv().await.unwrap();
    runtime.input.send(Input::Eof).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while runtime.tools.cancellations.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    tool.done.send(()).unwrap();
    let terminal = receive(&mut runtime.commits).await;
    assert_eq!(terminal.phase, "completedInterrupted");
    terminal.permit.send(true).unwrap();
    runtime.running.await.unwrap().unwrap();
    assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
    assert!(runtime.commits.try_recv().is_err());
}

#[tokio::test]
async fn unconfirmed_process_cleanup_stops_runtime_before_any_next_model_request() {
    let mut runtime = start(vec![call(0, "CleanupFailure")], None).await;
    let mut commits = runtime.commits;
    let database = tokio::spawn(async move {
        while let Some(commit) = commits.recv().await {
            let _ = commit.permit.send(true);
        }
    });
    let result = tokio::time::timeout(std::time::Duration::from_secs(3), runtime.running)
        .await
        .unwrap()
        .unwrap();
    assert!(
        result
            .unwrap_err()
            .to_string()
            .contains("fault.runtime.processCleanup")
    );
    assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.tools.calls.load(Ordering::SeqCst), 1);
    receive(&mut runtime.requests).await;
    assert!(runtime.requests.try_recv().is_err());
    drop(runtime.store);
    database.await.unwrap();
}

#[tokio::test]
async fn todo_state_and_result_commits_gate_next_tools_and_recover_actual_result() {
    for stage in ["state", "result", "recover"] {
        let todo_call = json!({"id":"todo-call","function":{"name":"TodoWrite","arguments":json!({"todos":[{"content":"durable todo","status":"in_progress","priority":"high"}]}).to_string()}});
        let mut runtime = start(vec![todo_call, call(2, "Write")], None).await;
        let state = loop {
            let c = receive(&mut runtime.commits).await;
            if c.session.as_ref().is_some_and(|s| !s.todos.is_empty()) {
                break c;
            }
            c.permit.send(true).unwrap();
        };
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.tools.calls.load(Ordering::SeqCst), 0);
        assert_eq!(state.last_role, "assistant");
        let saved = state.session.unwrap();
        assert_eq!(saved.todos.len(), 1);
        assert_eq!(saved.todos[0].content, "durable todo");
        state.permit.send(stage != "state").unwrap();
        if stage == "recover" {
            runtime.running.abort();
            let _ = runtime.running.await;
            let mut recovered = saved;
            recovered.recover("cold".into(), 2000);
            let result = recovered
                .messages
                .iter()
                .find(|m| m["tool_call_id"] == "todo-call")
                .unwrap();
            let data: Value = serde_json::from_str(result["content"].as_str().unwrap()).unwrap();
            assert_eq!(data["todos"][0]["content"], "durable todo");
            assert_eq!(recovered.todos.len(), 1);
            continue;
        }
        if stage == "result" {
            let c = receive(&mut runtime.commits).await;
            assert_eq!(c.last_role, "tool");
            assert_eq!(runtime.tools.calls.load(Ordering::SeqCst), 0);
            c.permit.send(false).unwrap();
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), runtime.running)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
        assert_eq!(runtime.model.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.tools.calls.load(Ordering::SeqCst), 0);
        assert!(runtime.commits.try_recv().is_err());
    }
}
