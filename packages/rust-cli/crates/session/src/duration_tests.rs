//! 回合结束那一行的用时（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）：会话 actor 在 `TurnFinished` 之后发
//! `TurnDuration`；中断与没发主线程请求的回合没有；等回答的时间不算；等后台子代理之后的那一行从第一个等待的回合算起。
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::FutureExt;
use futures_util::stream::{BoxStream, StreamExt};
use rust_cli_kernel_api::agents::{AgentSignal, AgentSignals};
use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use rust_cli_kernel_api::ids::{AgentId, SessionId};
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::session_api::{
    Command, KernelEvent, SequencedEvent, SessionHandle, TurnOutcome,
};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response, tool_use_response};
use rust_cli_kernel_api::turn_duration::TurnDuration;
use serde_json::json;
use tokio::sync::Notify;

use crate::test_support::{GatedModel, ScriptedHooks, Setup, envelope, submit};

/// 测试拨动的时钟；UTC 偏移固定是东八区。
struct ManualClock(AtomicU64);

impl ManualClock {
    fn at(ms: u64) -> Arc<Self> {
        Arc::new(Self(AtomicU64::new(ms)))
    }

    fn set(&self, ms: u64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

impl rust_cli_kernel_api::clock::Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.0.load(Ordering::SeqCst)
    }

    fn time_zone(&self) -> String {
        "Asia/Shanghai".to_owned()
    }

    fn utc_offset_minutes(&self, _at_ms: u64) -> i32 {
        480
    }
}

/// 后台子代理的替身：`background_agents` 给出测试设定的清单，没有信号。
#[derive(Default)]
struct Waiting(Mutex<Vec<AgentId>>);

impl Waiting {
    fn set(&self, agents: &[&str]) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) =
            agents.iter().map(|agent| AgentId::new(*agent)).collect();
    }
}

#[async_trait]
impl AgentSignals for Waiting {
    fn signals(&self, _session: &SessionId) -> BoxStream<'static, AgentSignal> {
        futures_util::stream::pending().boxed()
    }

    async fn ack(&self, _session: &SessionId, _seq: u64) {}

    async fn background(&self, _session: &SessionId) -> Vec<AgentId> {
        Vec::new()
    }

    async fn background_agents(&self, _session: &SessionId) -> Vec<AgentId> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

/// 读带序号的事件直到满足 `done`（5 s 内）。
async fn until(
    events: &mut BoxStream<'static, SequencedEvent>,
    done: impl Fn(&KernelEvent) -> bool,
) -> Vec<SequencedEvent> {
    let mut seen = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(5), events.next())
            .await
            .unwrap_or_else(|_| panic!("timed out; saw {seen:?}"))
            .unwrap();
        let stop = done(&event.event);
        seen.push(event);
        if stop {
            return seen;
        }
    }
}

fn started(event: &KernelEvent) -> bool {
    matches!(event, KernelEvent::TurnStarted { .. })
}

fn finished(event: &KernelEvent) -> bool {
    matches!(event, KernelEvent::TurnFinished { .. })
}

fn line(event: &KernelEvent) -> Option<&TurnDuration> {
    match event {
        KernelEvent::TurnDuration(line) => Some(line),
        _ => None,
    }
}

/// actor 处理完之前的消息之后，已经发出的事件（快照的查询排在回合收尾之后处理，回合结束那一行在它之前就发了）。
async fn drain(
    session: &crate::Session,
    events: &mut BoxStream<'static, SequencedEvent>,
) -> Vec<KernelEvent> {
    session.view().await.unwrap();
    let mut seen = Vec::new();
    while let Some(Some(event)) = events.next().now_or_never() {
        seen.push(event.event);
    }
    seen
}

#[tokio::test]
async fn a_finished_turn_reports_how_long_it_took_right_after_turn_finished() {
    let gate = Arc::new(Notify::new());
    let clock = ManualClock::at(1_000);
    let mut setup = Setup::new(Arc::new(GatedModel::new(gate.clone())));
    setup.clock = clock.clone();
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "hi")).await;
    until(&mut events, started).await;
    clock.set(63_000);
    gate.notify_one();
    let seen = until(&mut events, |event| line(event).is_some()).await;
    let [.., finished_event, duration] = seen.as_slice() else {
        panic!("{seen:?}");
    };
    assert!(
        matches!(
            finished_event.event,
            KernelEvent::TurnFinished {
                outcome: TurnOutcome::Success,
                ..
            }
        ),
        "{seen:?}"
    );
    let KernelEvent::TurnFinished { turn, .. } = &finished_event.event else {
        unreachable!();
    };
    assert_eq!(
        line(&duration.event),
        Some(&TurnDuration {
            turn: turn.clone(),
            duration_ms: 62_000,
            finished_at_ms: 63_000,
            utc_offset_minutes: 480,
            waiting_agents: 0,
        })
    );
    // 只供显示：不加 revision。
    assert_eq!(duration.revision, finished_event.revision);
}

#[tokio::test]
async fn an_interrupted_turn_has_no_duration_line() {
    let gate = Arc::new(Notify::new());
    let built = Setup::new(Arc::new(GatedModel::new(gate))).build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "hi")).await;
    until(&mut events, started).await;
    built.session.send(envelope("c2", Command::Interrupt)).await;
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last().map(|event| &event.event),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Interrupted,
            ..
        })
    ));
    let later = drain(&built.session, &mut events).await;
    assert!(later.iter().all(|event| line(event).is_none()), "{later:?}");
}

#[tokio::test]
async fn a_failed_request_still_gets_the_line() {
    let model = Arc::new(ScriptedModel::new(vec![vec![Err(
        ModelError::InvalidRequest {
            message: "bad request".to_owned(),
        },
    )]]));
    let built = Setup::new(model).build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "hi")).await;
    let seen = until(&mut events, |event| line(event).is_some()).await;
    assert!(
        seen.iter().any(|event| matches!(
            event.event,
            KernelEvent::TurnFinished {
                outcome: TurnOutcome::Failed { .. },
                ..
            }
        )),
        "{seen:?}"
    );
}

/// UserPromptSubmit 阻塞的输入与 `/compact` 都不发主线程的请求：没有这一行。
#[tokio::test]
async fn a_turn_without_a_main_thread_request_has_no_line() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["unused"])]));
    let mut setup = Setup::new(model.clone());
    setup.hooks = Arc::new(ScriptedHooks::with(
        HookEventName::UserPromptSubmit,
        HookOutcome {
            blocking_error: Some("no".into()),
            notices: vec!["UserPromptSubmit operation blocked by hook:\nno".into()],
            ..HookOutcome::default()
        },
    ));
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "secret")).await;
    until(&mut events, finished).await;
    built
        .session
        .send(envelope("c2", Command::Compact { instructions: None }))
        .await;
    until(&mut events, finished).await;
    let later = drain(&built.session, &mut events).await;
    assert!(model.requests().is_empty());
    assert!(later.iter().all(|event| line(event).is_none()), "{later:?}");
}

/// 等回答的 5 秒不算进用时（询问框打开时停表）。
#[tokio::test]
async fn time_spent_waiting_for_an_answer_is_left_out() {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "AskUserQuestion", &json!({"question": "which?"})),
        text_response(&["thanks"]),
    ]));
    let clock = ManualClock::at(1_000);
    let mut setup = Setup::new(model);
    setup.tools = vec![Arc::new(crate::tests::AskingTool)];
    setup.clock = clock.clone();
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    built.session.send(submit("c1", "ask me")).await;
    let interaction = crate::tests::asked(&mut events).await;
    clock.set(6_000);
    built
        .session
        .send(crate::tests::answer("c2", interaction))
        .await;
    let seen = until(&mut events, |event| line(event).is_some()).await;
    let duration = seen.iter().find_map(|event| line(&event.event)).unwrap();
    assert_eq!((duration.duration_ms, duration.finished_at_ms), (0, 6_000));
}

/// 一个由闸门放行的回合：`start` 时提交，开始之后把时钟拨到 `end` 再放行，返回这一行。
async fn gated_turn(
    built: &crate::test_support::Built,
    events: &mut BoxStream<'static, SequencedEvent>,
    (gate, clock): (&Notify, &ManualClock),
    id: &str,
    (start, end): (u64, u64),
) -> TurnDuration {
    clock.set(start);
    built.session.send(submit(id, "go")).await;
    until(events, started).await;
    clock.set(end);
    gate.notify_one();
    let seen = until(events, |event| line(event).is_some()).await;
    seen.iter()
        .find_map(|event| line(&event.event))
        .cloned()
        .unwrap()
}

/// 等后台子代理：第一行写等待（数还在跑的），之后第一个不用等的回合从第一个等待的回合开始算，再下一个回合照常。
#[tokio::test]
async fn after_waiting_for_background_agents_the_line_counts_from_the_first_wait() {
    let gate = Arc::new(Notify::new());
    let clock = ManualClock::at(1_000);
    let agents = Arc::new(Waiting::default());
    let mut setup = Setup::new(Arc::new(GatedModel::new(gate.clone())));
    setup.clock = clock.clone();
    setup.agents = agents.clone();
    let built = setup.build().await;
    let mut events = built.session.subscribe(None);
    let parts = (gate.as_ref(), clock.as_ref());
    agents.set(&["agent-a", "agent-b"]);
    let first = gated_turn(&built, &mut events, parts, "c1", (1_000, 4_000)).await;
    assert_eq!((first.waiting_agents, first.duration_ms), (2, 3_000));
    agents.set(&["agent-b"]);
    let second = gated_turn(&built, &mut events, parts, "c2", (5_000, 6_000)).await;
    assert_eq!((second.waiting_agents, second.duration_ms), (1, 1_000));
    agents.set(&[]);
    let third = gated_turn(&built, &mut events, parts, "c3", (10_000, 12_000)).await;
    assert_eq!((third.waiting_agents, third.duration_ms), (0, 11_000));
    let fourth = gated_turn(&built, &mut events, parts, "c4", (12_000, 13_000)).await;
    assert_eq!((fourth.waiting_agents, fourth.duration_ms), (0, 1_000));
}
