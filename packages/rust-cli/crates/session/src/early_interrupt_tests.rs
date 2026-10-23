//! 输入还没提交时被中断（修复线 F37，docs/specs/rust-cli/session-lifecycle.md §3.1）：中断或关闭落在展开、`@` 提及与
//! UserPromptSubmit 的空档里时，输入照常提交，主循环在发请求之前写中断标记；下一个回合的请求带着它们（处理输入
//! 之后不看中止）。UserPromptSubmit 停在闸门上、不放行，回合只能经中止走出 hook，所以中断一定落在空档里；
//! hook 开始等时发 `entered`，测试等它再中断（hook 正在跑）。旧代码以 `Aborted` 结束，什么都不提交。

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use rust_cli_kernel_api::hooks::{HookEventName, HookOutcome};
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::{AttachmentMessage, ContentBlock, Message};
use rust_cli_kernel_api::metering::CallStatus;
use rust_cli_kernel_api::orchestration::{
    BuiltContext, ContextBuilder, ContextRequest, OrchestrationError,
};
use rust_cli_kernel_api::session_api::{
    Command, EndReason, KernelEvent, SessionHandle, TurnOutcome,
};
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::testing::{ScriptedModel, text_response};
use tokio::sync::Notify;

use crate::test_support::{ScriptedHooks, Setup, envelope, finished, submit, until};

const MARKER: &str = "[Request interrupted by user]";

/// 每次调用的 ID 不同：当步的 attachment 是 `turn-N`，上下文宣布是 `date-N`。
#[derive(Default)]
struct Counted {
    turns: AtomicUsize,
    builds: AtomicUsize,
}

fn attachment(id: String, source: &str) -> AttachmentMessage {
    AttachmentMessage {
        id: MessageId::new(id.clone()),
        source: source.to_owned(),
        text: format!("<system-reminder>\n{id}\n</system-reminder>"),
        data: serde_json::Value::Null,
    }
}

#[async_trait]
impl ContextBuilder for Counted {
    async fn build(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<BuiltContext, OrchestrationError> {
        let n = self.builds.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(BuiltContext {
            system: Vec::new(),
            user_context: Vec::new(),
            attachments: vec![attachment(format!("date-{n}"), "date")],
        })
    }

    async fn turn_attachments(
        &self,
        _request: ContextRequest,
        _history: &[Message],
    ) -> Result<Vec<AttachmentMessage>, OrchestrationError> {
        let n = self.turns.fetch_add(1, Ordering::SeqCst) + 1;
        Ok(vec![attachment(
            format!("turn-{n}"),
            "total_tokens_reminder",
        )])
    }
}

/// UserPromptSubmit 停在闸门上（结果为空）；返回闸门与“hook 开始等了”的通知。
fn held_hooks() -> (Arc<ScriptedHooks>, Arc<Notify>, Arc<Notify>) {
    let gate = Arc::new(Notify::new());
    let entered = Arc::new(Notify::new());
    let hooks = ScriptedHooks {
        gate: Some(gate.clone()),
        entered: Some(entered.clone()),
        ..ScriptedHooks::default()
    };
    hooks.set(HookEventName::UserPromptSubmit, HookOutcome::default());
    (Arc::new(hooks), gate, entered)
}

/// 消息的文字：user 的第一个文本块，attachment 的 ID。
fn labels(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .map(|message| match message {
            Message::User(user) => match user.content.first() {
                Some(ContentBlock::Text { text }) => text.clone(),
                other => format!("{other:?}"),
            },
            other => other.id().as_str().to_owned(),
        })
        .collect()
}

/// `needle` 在请求的消息（JSON）里第一次出现的位置。
fn position(json: &str, needle: &str) -> usize {
    json.find(needle)
        .unwrap_or_else(|| panic!("{needle} is not in the request: {json}"))
}

/// esc：输入、当步的 attachment、上下文宣布、中断标记依次落库，不调模型；下一个回合的请求按这个顺序带着它们。
#[tokio::test]
async fn an_interrupt_during_the_prompt_hook_keeps_the_input_and_marks_it() {
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["second reply"])]));
    let (hooks, gate, entered) = held_hooks();
    let mut setup = Setup::new(model.clone());
    setup.hooks = hooks;
    setup.context = Some(Arc::new(Counted::default()));
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "first")).await;
    entered.notified().await;
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
    let view = built.session.view().await.unwrap();
    assert_eq!(
        labels(&view.messages),
        ["first", "turn-1", "date-1", MARKER],
        "the input is committed before the interruption marker"
    );
    assert!(
        model.requests().is_empty(),
        "no request for an aborted turn"
    );

    // 下一条输入：hook 放行一次。
    gate.notify_one();
    built.session.send(submit("c3", "second")).await;
    let seen = until(&mut events, finished).await;
    assert!(matches!(
        seen.last(),
        Some(KernelEvent::TurnFinished {
            outcome: TurnOutcome::Success,
            ..
        })
    ));
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    // attachment 可能按角色发成对话中间的 system 消息（context.md §3），只比人类输入一侧的先后。
    let json = serde_json::to_string(&requests[0].messages).unwrap();
    let order: Vec<usize> = ["\"text\":\"first", MARKER, "\"text\":\"second"]
        .iter()
        .map(|needle| position(&json, needle))
        .collect();
    assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{json}");
    for announced in ["turn-1", "date-1"] {
        position(&json, announced);
    }
}

/// 关闭（`/clear`、`/resume` 换会话）同样中止回合：输入与中断标记落库，这一轮记成被中断。
#[tokio::test]
async fn closing_during_the_prompt_hook_keeps_the_input_and_marks_it() {
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let (hooks, _gate, entered) = held_hooks();
    let mut setup = Setup::new(model.clone());
    setup.hooks = hooks;
    let built = setup.build().await;
    built.session.send(submit("c1", "first")).await;
    entered.notified().await;
    built.session.close(EndReason::Clear).await;
    let facts = built.store.facts(&SessionId::new("sess_1"));
    let messages: Vec<Message> = facts
        .iter()
        .filter_map(|fact| match fact {
            Fact::MessageAppended { message } => Some(message.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(labels(&messages), ["first", MARKER]);
    assert!(
        facts.iter().any(|fact| matches!(
            fact,
            Fact::TurnEnded {
                status: CallStatus::Cancelled
            }
        )),
        "{facts:?}"
    );
    assert!(model.requests().is_empty());
}

/// 退出（退出信号的 `Shutdown`、TUI 退出与 `-p` 结束的关闭，修复线 F38，session-lifecycle.md §3.5）：收到退出信号
/// 不中止 query、直接退出，这条输入还没进对话——什么都不写（没有输入、标记与回合计量），不调模型。F37 时同 esc。
#[tokio::test]
async fn exiting_during_the_prompt_hook_records_nothing() {
    for shutdown_first in [true, false] {
        let model = Arc::new(ScriptedModel::new(Vec::new()));
        let (hooks, _gate, entered) = held_hooks();
        let mut setup = Setup::new(model.clone());
        setup.hooks = hooks;
        let built = setup.build().await;
        built.session.send(submit("c1", "first")).await;
        entered.notified().await;
        if shutdown_first {
            built.session.send(envelope("c2", Command::Shutdown)).await;
        }
        built.session.close(EndReason::Other).await;
        let facts = built.store.facts(&SessionId::new("sess_1"));
        assert!(
            !facts
                .iter()
                .any(|fact| matches!(fact, Fact::MessageAppended { .. } | Fact::TurnEnded { .. })),
            "{facts:?}"
        );
        assert!(model.requests().is_empty());
    }
}
