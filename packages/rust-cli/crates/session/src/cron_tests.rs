//! 定时任务的触发（docs/specs/rust-cli/tools-more.md §10.4）：空闲时唤醒即开回合；回合进行中的唤醒
//! 忽略，回合结束后排在其他输入之后取走到期任务。

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use rust_cli_kernel_api::cron::{CronFire, CronSignals};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::session_api::{KernelEvent, SessionHandle};
use rust_cli_kernel_api::testing::{FakeTool, ScriptedModel, text_response, tool_use_response};
use serde_json::json;
use tokio::sync::{Notify, mpsc};
use tokio_stream_shim::ReceiverStream;

use crate::test_support::{Setup, finished, submit, until};

/// 测试用的定时任务来源：唤醒由测试发出，`take_due` 交出预先放好的任务并记下被调用的次数。
struct ScriptedCron {
    wakeups: Mutex<Option<mpsc::UnboundedReceiver<()>>>,
    due: Mutex<VecDeque<CronFire>>,
    taken: Mutex<u32>,
    /// 还排着的 loop 唤醒数（`cancel_loops` 清零并返回它）与 `cancel_loops` 被调用的次数。
    loops: Mutex<usize>,
    cancels: Mutex<u32>,
    /// `resume` 收到的历史条数（每次调用一项）。
    resumed: Mutex<Vec<usize>>,
}

impl ScriptedCron {
    fn new() -> (Arc<Self>, mpsc::UnboundedSender<()>) {
        let (sender, receiver) = mpsc::unbounded_channel();
        let cron = Arc::new(Self {
            wakeups: Mutex::new(Some(receiver)),
            due: Mutex::default(),
            taken: Mutex::new(0),
            loops: Mutex::new(0),
            cancels: Mutex::new(0),
            resumed: Mutex::default(),
        });
        (cron, sender)
    }

    fn due(&self, prompt: &str) {
        self.due_as(prompt, rust_cli_kernel_api::cron::CronFireKind::Task);
    }

    fn due_as(&self, prompt: &str, kind: rust_cli_kernel_api::cron::CronFireKind) {
        self.due
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push_back(CronFire {
                job: "1a2b3c4d".to_owned(),
                prompt: prompt.to_owned(),
                kind,
            });
    }
}

#[async_trait]
impl CronSignals for ScriptedCron {
    fn wakeups(&self, _session: &SessionId) -> BoxStream<'static, ()> {
        let receiver = self
            .wakeups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        match receiver {
            Some(receiver) => ReceiverStream(receiver).boxed(),
            None => futures_util::stream::pending().boxed(),
        }
    }

    async fn take_due(&self, _session: &SessionId) -> Vec<CronFire> {
        *self.taken.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        self.due
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .drain(..)
            .collect()
    }

    async fn resume(&self, _session: &SessionId, history: &[Message]) {
        self.resumed
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(history.len());
    }

    async fn cancel_loops(&self, _session: &SessionId) -> usize {
        *self.cancels.lock().unwrap_or_else(PoisonError::into_inner) += 1;
        std::mem::take(&mut *self.loops.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

/// `UnboundedReceiver` 作为 Stream（不为测试引入 tokio-stream）。
mod tokio_stream_shim {
    use std::pin::Pin;
    use std::task::{Context, Poll};

    pub(super) struct ReceiverStream(pub(super) tokio::sync::mpsc::UnboundedReceiver<()>);

    impl futures_util::Stream for ReceiverStream {
        type Item = ();

        fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<()>> {
            self.0.poll_recv(cx)
        }
    }
}

fn cron_prompts(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user)
                if matches!(&user.origin, UserOrigin::Meta { source } if source == "scheduled-task") =>
            {
                match user.content.as_slice() {
                    [ContentBlock::Text { text }] => Some(text.clone()),
                    _ => None,
                }
            }
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn an_idle_session_runs_a_due_job_as_a_meta_prompt() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["checked"])]));
    let (cron, wake) = ScriptedCron::new();
    let mut setup = Setup::new(model.clone());
    setup.cron = cron.clone();
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    cron.due("check the deploy");
    wake.send(()).unwrap();
    let seen = until(&mut events, finished).await;
    assert!(
        seen.iter()
            .any(|event| matches!(event, KernelEvent::TurnStarted { command: None, .. }))
    );
    let view = built.session.view().await.unwrap();
    assert_eq!(cron_prompts(&view.messages), ["check the deploy"]);
    assert_eq!(model.requests().len(), 1);
}

#[tokio::test]
async fn a_busy_session_runs_due_jobs_after_queued_input() {
    let gate = Arc::new(Notify::new());
    let tool = Arc::new(FakeTool::new("Slow", false, "done").gated(gate.clone()));
    let started = tool.started.clone();
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Slow", &json!({})),
        text_response(&["first"]),
        text_response(&["queued"]),
        text_response(&["cron"]),
    ]));
    let (cron, wake) = ScriptedCron::new();
    let mut setup = Setup::new(model.clone());
    setup.cron = cron.clone();
    setup.tools = vec![tool];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "start")).await;
    started.notified().await;
    // 回合进行中：唤醒被忽略，不取任务。
    cron.due("tick");
    wake.send(()).unwrap();
    built.session.send(submit("c2", "next")).await;
    tokio::task::yield_now().await;
    assert_eq!(
        *cron.taken.lock().unwrap_or_else(PoisonError::into_inner),
        0
    );
    gate.notify_one();
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    // 顺序：排队的用户输入先开回合，定时任务在它之后。
    let users: Vec<String> = view
        .messages
        .iter()
        .filter_map(|message| match message {
            Message::User(user) => match user.content.as_slice() {
                [ContentBlock::Text { text }] => Some(text.clone()),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(users, ["start", "next", "tick"]);
    assert_eq!(cron_prompts(&view.messages), ["tick"]);
}

/// 用户中断取消 loop 唤醒（tools-more.md §18.1）：空闲时取消了才算接受；运行中先取消再中止回合。
#[tokio::test]
async fn an_interrupt_cancels_loop_wakeups_first() {
    use rust_cli_kernel_api::session_api::{Ack, Command, TurnOutcome};

    use crate::test_support::envelope;

    let (cron, _wake) = ScriptedCron::new();
    let gate = Arc::new(Notify::new());
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "Wait", &json!({})),
        text_response(&["late"]),
    ]));
    let mut setup = Setup::new(model);
    setup.cron = cron.clone();
    setup.tools = vec![Arc::new(FakeTool::new("Wait", true, "done").gated(gate))];
    let built = setup.build().await;
    let session = &built.session;
    // 空闲：有两个唤醒 → 取消、接受；再按一次没有可取消的 → 照旧拒绝。
    *cron.loops.lock().unwrap_or_else(PoisonError::into_inner) = 2;
    assert!(matches!(
        session.send(envelope("i1", Command::Interrupt)).await,
        Ack::Accepted { .. }
    ));
    assert!(matches!(
        session.send(envelope("i2", Command::Interrupt)).await,
        Ack::Rejected { .. }
    ));
    assert_eq!(
        *cron.loops.lock().unwrap_or_else(PoisonError::into_inner),
        0
    );
    // 运行中：先取消唤醒，再中止回合。
    let mut events = session.subscribe(Some(0));
    session.send(submit("c1", "go")).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::ToolStarted { .. })
    })
    .await;
    *cron.loops.lock().unwrap_or_else(PoisonError::into_inner) = 1;
    session.send(envelope("i3", Command::Interrupt)).await;
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Interrupted,
            ..
        })
    ));
    assert_eq!(
        *cron.loops.lock().unwrap_or_else(PoisonError::into_inner),
        0
    );
    assert_eq!(
        *cron.cancels.lock().unwrap_or_else(PoisonError::into_inner),
        3
    );
}

/// resume（tools-more.md §18.2）：从存储打开的会话在进入循环前把活动历史交给任务表复原会话任务；新建的
/// 会话不调。
#[tokio::test]
async fn a_loaded_session_hands_its_history_to_the_task_table_first() {
    use rust_cli_kernel_api::ids::MessageId;
    use rust_cli_kernel_api::message::UserMessage;

    use crate::Opened;

    let user = |id: &str| {
        Message::User(UserMessage {
            id: MessageId::new(id),
            content: vec![ContentBlock::Text {
                text: "hi".to_owned(),
            }],
            origin: UserOrigin::Human,
        })
    };
    for (opened, expected) in [(Opened::Loaded, vec![2]), (Opened::Created, vec![])] {
        let (cron, _wake) = ScriptedCron::new();
        let mut setup = Setup::new(Arc::new(ScriptedModel::new(Vec::new())));
        setup.cron = cron.clone();
        setup.opened = opened;
        setup.messages = vec![user("u1"), user("u2")];
        let built = setup.build().await;
        // view 由 actor 回答：到这里时它已经进入循环，resume 一定已经做完。
        built.session.view().await.unwrap();
        assert_eq!(
            *cron.resumed.lock().unwrap_or_else(PoisonError::into_inner),
            expected
        );
    }
}

/// 触发时的界面行与 `/loop` 的 noop 折叠（tools-more.md §18.3）：第二次 loop 唤醒前模型报告没有动作，
/// 折叠说明排在 prompt 之前进模型上下文；普通任务只有界面行；错过提示没有界面行。
#[tokio::test]
async fn fires_announce_themselves_and_quiet_loop_ticks_fold() {
    use rust_cli_kernel_api::cron::{CronFireKind, LoopFold};

    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response("toolu_1", "ScheduleWakeup", &json!({"noop": true})),
        text_response(&["nothing to do"]),
        text_response(&["still nothing"]),
        text_response(&["ran the job"]),
        text_response(&["asked"]),
    ]));
    let (cron, wake) = ScriptedCron::new();
    let mut setup = Setup::new(model.clone());
    setup.cron = cron.clone();
    setup.tools = vec![Arc::new(FakeTool::new("ScheduleWakeup", true, "scheduled"))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    let mut fires = Vec::new();
    for (prompt, kind) in [
        ("/loop check", CronFireKind::Loop),
        ("/loop check", CronFireKind::Loop),
        ("run the job", CronFireKind::Task),
        ("missed", CronFireKind::Notice),
    ] {
        cron.due_as(prompt, kind);
        wake.send(()).unwrap();
        let seen = until(&mut events, finished).await;
        fires.extend(seen.into_iter().filter_map(|event| match event {
            KernelEvent::ScheduledFire { at, kind, fold } => Some((at, kind, fold)),
            _ => None,
        }));
    }
    let at = "Jan 1 12:00am".to_owned();
    assert_eq!(
        fires,
        [
            (at.clone(), CronFireKind::Loop, None),
            (
                at.clone(),
                CronFireKind::Loop,
                Some(LoopFold {
                    streak: 1,
                    since: at.clone()
                })
            ),
            (at, CronFireKind::Task, None),
        ]
    );
    // 第二次 loop 唤醒的请求里，折叠说明紧挨在 prompt 之前。
    let request = format!("{:?}", model.requests()[2].messages);
    let note = request
        .find("[1 prior /loop wakeup found nothing actionable; loop is healthy.]")
        .expect("the fold note reaches the model");
    assert!(note < request.rfind("/loop check").unwrap(), "{request}");
}
