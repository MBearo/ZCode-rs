//! 登记处的现场（tui-cockpit.md §4）：内容变了才写。在跑的子代理的用时随时间走，不算内容（读的人按 `atMs` 外推），
//! 所以子代理跑着、快照一次次到达时不重写；它结束了（`running` 变了）照常写。内核是 LocalKernel，子代理由 Agent 工具
//! 真实地派生，跨会话的端口是记下每次 `set_presence` 的替身，内核的时钟由测试拨动。
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_base::sync::LockUnpoisoned;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::model::{ModelPort, ModelRequest, QuerySource};
use rust_cli_kernel_api::peers::{
    PeerAddress, PeerIdentity, PeerMessaging, PeerOutbound, PeerPresence, PeerRecord,
    PeerSendError, PeerSignal, PeerStatus, ReceiptStatus,
};
use rust_cli_kernel_api::testing::{ManualClock, text_response, tool_use_response};
use serde_json::json;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use super::support::{Item, Setup, with_ui};

/// 子代理的步数：每步一个工具调用（一次提交、一份新快照），最后一步回文字结束。
const STEPS: usize = 3;

/// 记下每次写进登记的现场。
#[derive(Default)]
struct Registry {
    written: Mutex<Vec<PeerPresence>>,
}

impl Registry {
    fn written(&self) -> Vec<PeerPresence> {
        self.written.lock_unpoisoned().clone()
    }
}

#[async_trait]
impl PeerMessaging for Registry {
    async fn identity(&self) -> Option<PeerIdentity> {
        None
    }
    async fn peers(&self) -> Vec<PeerRecord> {
        Vec::new()
    }
    async fn send(&self, _message: PeerOutbound) -> Result<(), PeerSendError> {
        Ok(())
    }
    async fn receipt(&self, _to: PeerAddress, _msg_id: String, _status: ReceiptStatus) {}
    async fn bind(&self, _session: &SessionId) {}
    async fn unbind(&self, _session: &SessionId) {}
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, PeerSignal> {
        stream::pending().boxed()
    }
    async fn ack(&self, _session: &SessionId, _seq: u64) {}
    async fn set_status(&self, _session: &SessionId, _status: PeerStatus) {}
    async fn set_presence(&self, _session: &SessionId, presence: PeerPresence) {
        self.written.lock_unpoisoned().push(presence);
    }
}

/// 主线程派生一个后台子代理后回文字；子代理第 k 步先等闸门 k，前 `STEPS` 步各调一次 Read，之后回文字。
struct Model {
    main: Mutex<VecDeque<Vec<Item>>>,
    gates: Vec<(watch::Sender<bool>, watch::Receiver<bool>)>,
    /// 子代理发出的请求数。
    asked: AtomicUsize,
}

impl Model {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            main: Mutex::new(VecDeque::from([
                tool_use_response(
                    "toolu_main",
                    "Agent",
                    &json!({"description": "Probe task", "prompt": "KEY-PPP do the work.",
                            "subagent_type": "general-purpose", "run_in_background": true}),
                ),
                text_response(&["Launched."]),
            ])),
            gates: (0..=STEPS).map(|_| watch::channel(false)).collect(),
            asked: AtomicUsize::new(0),
        })
    }

    fn release(&self, step: usize) {
        self.gates[step].0.send_replace(true);
    }
}

impl ModelPort for Model {
    fn stream(&self, request: ModelRequest, cancel: CancellationToken) -> BoxStream<'static, Item> {
        let first = serde_json::to_string(&request.messages.first()).unwrap();
        let reply = match request.source {
            QuerySource::MainThread => {
                let next = self.main.lock().unwrap().pop_front();
                stream::iter(next.unwrap_or_else(|| text_response(&["Noted."]))).boxed()
            }
            QuerySource::Subagent(_) if first.contains("KEY-PPP") => {
                let step = self.asked.fetch_add(1, Ordering::SeqCst);
                let events = if step < STEPS {
                    tool_use_response(
                        &format!("toolu_p_{step}"),
                        "Read",
                        &json!({"file_path": format!("/Users/dev/project/p{step}.txt")}),
                    )
                } else {
                    text_response(&["P done."])
                };
                let mut open = self.gates[step].1.clone();
                stream::once(async move {
                    let _ = open.wait_for(|open| *open).await;
                })
                .flat_map(move |()| stream::iter(events.clone()))
                .boxed()
            }
            _ => stream::iter(text_response(&["Fixture side reply"])).boxed(),
        };
        reply.take_until(cancel.cancelled_owned()).boxed()
    }
}

/// 等到条件成立（不在画面上的事实）；等待有上限，只为让失败的测试结束。
async fn until(ui: &super::support::Ui, what: &str, done: impl Fn() -> bool) {
    let wait = async {
        while !done() {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!("timed out waiting for {what}; screen:\n{}", ui.screen());
    }
}

/// 比较用的内容：在跑的代理的用时与写的时刻去掉。
fn content(presence: &PeerPresence) -> PeerPresence {
    let mut presence = presence.clone();
    for agent in &mut presence.agents {
        agent.at_ms = 0;
        if agent.running {
            agent.elapsed_ms = 0;
        }
    }
    presence
}

/// 修复前：每份快照的用时都不同，runner 每轮循环都重写登记（实测在跑的子代理每秒约 4 次，每个看着 peers 目录的 TUI 跟着
/// 重读会话与笔记）。现在相邻两次写的内容总是不同；子代理结束时写出 `running = false` 与最后的用时。
#[tokio::test(flavor = "multi_thread")]
async fn a_running_subagent_does_not_rewrite_the_registry_as_time_passes() {
    let registry = Arc::new(Registry::default());
    let clock = Arc::new(ManualClock::new(1_000_000));
    let model = Model::new();
    let setup = Setup {
        width: 120,
        height: 32,
        clock: Some(clock.clone()),
        peers: rust_cli_runtime::PeerDeps {
            port: registry.clone(),
            hold_timeout_ms: 300_000,
            receipts_wake: true,
        },
        ..Setup::default()
    };
    let probe = model.clone();
    let written = registry.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.submit("go");
        ui.wait_for("Launched.").await;
        // 每一步：时钟走 11 s、放行，等子代理发出下一个请求（上一步的工具结果已提交，快照已重取）。
        for step in 0..STEPS {
            clock.advance(11_000);
            probe.release(step);
            until(&ui, "the next subagent request", || {
                probe.asked.load(Ordering::SeqCst) > step + 1
            })
            .await;
        }
        clock.advance(11_000);
        probe.release(STEPS);
        // 结束了：写出不在跑的那一份。
        until(&ui, "the finished presence", || {
            written
                .written()
                .iter()
                .any(|p| p.agents.first().is_some_and(|agent| !agent.running))
        })
        .await;
    })
    .await;

    let written = registry.written();
    let with_agent: Vec<_> = written.iter().filter(|p| !p.agents.is_empty()).collect();
    assert!(
        with_agent.iter().any(|p| p.agents[0].running),
        "the running subagent was never written: {written:#?}"
    );
    for pair in written.windows(2) {
        assert_ne!(
            content(&pair[0]),
            content(&pair[1]),
            "rewrote the registry with the same content: {written:#?}"
        );
    }
    let last = with_agent.last().unwrap();
    assert!(!last.agents[0].running);
    assert_eq!(last.agents[0].elapsed_ms, 44_000);
}
