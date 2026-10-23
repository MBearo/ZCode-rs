//! esc 撤回刚发的输入（修复线 F38，docs/specs/rust-cli/session-lifecycle.md §5.5）：
//! `Command::RestoreCancelled` 在 actor 的一次处理里判断“空闲、最后一条可选输入之后没有回答”，回退对话（`RevertSet`，
//! Node 的 `session.revert`）并带回这条输入。模型第一次被调用时先流出给定的事件，再等到被取消，测试在事件流出之后
//! 中断，所以中断一定落在这个进度上。旧代码没有这个命令（`Rejected`），撤回不了。
use std::sync::{Arc, Mutex, PoisonError};

use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message, Usage};
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelEvent, ModelPort, ModelRequest};
use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent, SessionHandle, TurnOutcome};
use rust_cli_kernel_api::store::{Fact, SessionStore};
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

use crate::test_support::{Setup, envelope, finished, submit, until};

const MARKER: &str = "[Request interrupted by user]";

/// 第一次调用：流出 `first` 之后发 `streamed`，等到被取消；之后的调用交给 `then`。
struct Partial {
    first: Mutex<Option<Vec<ModelEvent>>>,
    streamed: Arc<Notify>,
    then: ScriptedModel,
}

impl Partial {
    fn new(first: Vec<ModelEvent>, then: Vec<Vec<Result<ModelEvent, ModelError>>>) -> Arc<Self> {
        Arc::new(Self {
            first: Mutex::new(Some(first)),
            streamed: Arc::new(Notify::new()),
            then: ScriptedModel::new(then),
        })
    }
}

impl ModelPort for Partial {
    fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        let first = self
            .first
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(events) = first else {
            return self.then.stream(request, cancel);
        };
        let streamed = self.streamed.clone();
        let hang = stream::once(async move {
            streamed.notify_one();
            cancel.cancelled().await;
            Err(ModelError::Cancelled)
        });
        stream::iter(events.into_iter().map(Ok)).chain(hang).boxed()
    }
}

fn start() -> ModelEvent {
    ModelEvent::MessageStart {
        id: "msg_1".to_owned(),
        model: "scripted".to_owned(),
        usage: Usage::default(),
    }
}

fn open(index: usize, block: ContentBlock) -> ModelEvent {
    ModelEvent::BlockStart { index, block }
}

fn delta(index: usize, delta: BlockDelta) -> ModelEvent {
    ModelEvent::BlockDelta { index, delta }
}

fn text_of(message: &Message) -> String {
    match message {
        Message::User(user) => user
            .content
            .iter()
            .find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
            .unwrap_or_default(),
        Message::Assistant(assistant) => format!("assistant:{:?}", assistant.content),
        other => other.id().as_str().to_owned(),
    }
}

/// 发一条输入，等模型流出 `first`，esc 中断，等回合以 `Interrupted` 结束，再请求撤回。
async fn interrupt_then_restore(model: Arc<Partial>) -> (crate::test_support::Built, Ack) {
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    model.streamed.notified().await;
    built.session.send(envelope("c2", Command::Interrupt)).await;
    let seen = until(&mut events, finished).await;
    assert!(
        matches!(
            seen.last(),
            Some(KernelEvent::TurnFinished {
                outcome: TurnOutcome::Interrupted,
                ..
            })
        ),
        "{seen:?}"
    );
    let ack = built
        .session
        .send(envelope("c3", Command::RestoreCancelled))
        .await;
    (built, ack)
}

/// 请求已发、还没有第一个字时 esc：对话回退到这条输入之前（库里是 `RevertSet`，冷读取没有这一轮），回执带回
/// 这条输入；再发一次，请求里只有这条输入一次、没有中断标记（session-lifecycle.md §5.5）。
#[tokio::test]
async fn an_input_interrupted_before_any_text_is_rewound_and_returned() {
    let model = Partial::new(vec![start()], vec![text_response(&["again"])]);
    let (built, ack) = interrupt_then_restore(model.clone()).await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}");
    };
    let message: Message = serde_json::from_value(result["message"].clone()).unwrap();
    assert_eq!(text_of(&message), "first");
    assert!(result["removed"].as_u64().unwrap() >= 2, "{result}");
    let view = built.session.view().await.unwrap();
    assert!(view.messages.is_empty(), "{:?}", view.messages);
    let id = SessionId::new("sess_1");
    assert!(
        built
            .store
            .facts(&id)
            .iter()
            .any(|fact| matches!(fact, Fact::RevertSet { .. }))
    );
    assert!(built.store.load(&id).await.unwrap().messages.is_empty());

    let mut events = built.session.subscribe(Some(view.last_seq));
    built.session.send(submit("c4", "first")).await;
    until(&mut events, finished).await;
    let requests = model.then.requests();
    assert_eq!(requests.len(), 1);
    let json = serde_json::to_string(&requests[0].messages).unwrap();
    assert_eq!(json.matches("\"text\":\"first").count(), 1, "{json}");
    assert!(!json.contains(MARKER), "{json}");
}

/// 只流出 thinking、或只有空白的文字：都不算回答（只认非空白文字与 tool_use），照样撤回。
#[tokio::test]
async fn thinking_or_blank_text_is_no_answer() {
    let thinking = open(
        0,
        ContentBlock::Thinking {
            thinking: String::new(),
            signature: None,
        },
    );
    let blank = open(
        0,
        ContentBlock::Text {
            text: String::new(),
        },
    );
    for first in [
        vec![
            start(),
            thinking,
            delta(0, BlockDelta::Thinking("hmm".into())),
        ],
        vec![start(), blank, delta(0, BlockDelta::Text("  \n".into()))],
    ] {
        let (_built, ack) = interrupt_then_restore(Partial::new(first, Vec::new())).await;
        assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    }
}

/// 流出了文字（块还没结束）时 esc：半句作为 assistant 留下（query.md §1.4），之后是中断标记；这条输入回答过，
/// 撤回被拒绝。旧代码丢掉没写完的块：对话里只有输入与标记。
#[tokio::test]
async fn streamed_text_is_kept_and_answers_the_input() {
    let first = vec![
        start(),
        open(
            0,
            ContentBlock::Text {
                text: String::new(),
            },
        ),
        delta(0, BlockDelta::Text("Partial answer".into())),
    ];
    let (built, ack) = interrupt_then_restore(Partial::new(first, Vec::new())).await;
    assert!(matches!(ack, Ack::Rejected { .. }), "{ack:?}");
    let view = built.session.view().await.unwrap();
    let texts: Vec<String> = view.messages.iter().map(text_of).collect();
    assert_eq!(texts.len(), 3, "{texts:?}");
    assert_eq!(texts[0], "first");
    assert!(texts[1].contains("Partial answer"), "{texts:?}");
    assert_eq!(texts[2], MARKER);
}

/// 回合还在跑时不撤回（actor 一次判断：TUI 判断之后内核可能已经开了下一个回合）。
#[tokio::test]
async fn a_running_turn_is_not_restored() {
    let model = Partial::new(vec![start()], Vec::new());
    let built = Setup::new(model.clone()).build().await;
    built.session.send(submit("c1", "first")).await;
    model.streamed.notified().await;
    let ack = built
        .session
        .send(envelope("c2", Command::RestoreCancelled))
        .await;
    assert!(matches!(ack, Ack::Rejected { .. }), "{ack:?}");
}

/// 退出（`Shutdown`，session-lifecycle.md §3.5）时流出的半句不留、也没有中断标记（收到退出信号不中止 query），
/// 之后不再开回合（排队的输入留着）。
#[tokio::test]
async fn a_shutdown_while_streaming_keeps_no_partial_text_and_no_marker() {
    let first = vec![
        start(),
        open(
            0,
            ContentBlock::Text {
                text: String::new(),
            },
        ),
        delta(0, BlockDelta::Text("Partial answer".into())),
    ];
    let model = Partial::new(first, vec![text_response(&["queued reply"])]);
    let built = Setup::new(model.clone()).build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    model.streamed.notified().await;
    built.session.send(submit("c2", "second")).await;
    let ack = built.session.send(envelope("c3", Command::Shutdown)).await;
    assert!(matches!(ack, Ack::Accepted { .. }), "{ack:?}");
    until(&mut events, finished).await;
    let view = built.session.view().await.unwrap();
    let texts: Vec<String> = view.messages.iter().map(text_of).collect();
    assert_eq!(texts, ["first"], "nothing after the input");
    assert!(!view.running, "no queued turn starts after a shutdown");
    assert_eq!(view.queue.len(), 1);
    assert!(model.then.requests().is_empty());
}

/// bash 模式的命令运行中 esc（`! sleep 30` 撤回到输入框、回到 bash 模式）：`<bash-input>` 是可选的输入，之后只有
/// 中断标记，撤回；回执带回 `<bash-input>` 消息，界面据此回到 bash 模式（tui-input.md §8）。排在输入之前的 caveat（meta）
/// 留下（transcript 同样只剩它）。
#[tokio::test]
async fn an_interrupted_bash_command_is_restored() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let mut setup = Setup::new(model);
    setup.inputs = Arc::new(crate::input_tests::FakeInputs);
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    let bash = Command::Bash {
        command: "slow".to_owned(),
        delivery: rust_cli_kernel_api::session_api::Delivery::Queue,
    };
    built.session.send(envelope("c1", bash)).await;
    until(&mut events, |event| {
        matches!(event, KernelEvent::TurnStarted { .. })
    })
    .await;
    built.session.send(envelope("c2", Command::Interrupt)).await;
    until(&mut events, finished).await;
    let ack = built
        .session
        .send(envelope("c3", Command::RestoreCancelled))
        .await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}");
    };
    let message: Message = serde_json::from_value(result["message"].clone()).unwrap();
    assert_eq!(text_of(&message), "<bash-input>slow</bash-input>");
    let view = built.session.view().await.unwrap();
    let texts: Vec<String> = view.messages.iter().map(text_of).collect();
    assert_eq!(texts.len(), 1, "{texts:?}");
    assert!(texts[0].starts_with("<local-command-caveat>"), "{texts:?}");
}
