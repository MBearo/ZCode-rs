//! 会话关闭（docs/specs/rust-cli/session-lifecycle.md §3.3）：actor 中止前台工作后退出，释放执行环境里的会话；
//! 关闭之后到达的后台通知不再开回合，留在执行环境里（不 ack），会话重新打开时送达。

use rust_cli_kernel_api::session_api::EndReason;
use rust_cli_kernel_api::testing::{NoticeEnvironment, shell_notice};

use super::*;

/// 复现（R1-1）：以前 `close` 只跑 SessionEnd，actor 还活着——之后到达的后台通知让它在用户看不到的会话里
/// 自己开回合（花 token），`view` 照常回答，执行环境也收不到 `close_session`。
#[tokio::test]
async fn a_closed_session_stops_its_actor_and_leaves_later_notices_alone() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["handled"])]));
    let session =
        super::notice::session(&scope, env.clone(), model.clone(), Vec::new(), Vec::new()).await;
    // 先确认 actor 在回答（订阅了通知流）。
    session.view().await.unwrap();

    session.close(EndReason::Clear).await;
    env.finish(shell_notice(1, "build"));

    assert!(
        session.view().await.is_err(),
        "the actor must have stopped after close"
    );
    assert_eq!(
        env.closed(),
        [(SessionId::new("sess_1"), false)],
        "/clear keeps the background work, so the environment does not forget the session"
    );
    assert!(
        env.acked().is_empty(),
        "the notice waits for the session to be opened again"
    );
    assert!(
        model.requests().is_empty(),
        "no turn was started after close"
    );
}

/// 退出（TUI 退出、`-p` 结束）时执行环境释放这个会话的全部状态（`forget`）。
#[tokio::test]
async fn leaving_the_program_forgets_the_session_in_the_environment() {
    let scope = TaskScope::root("test", |_| {});
    let env = Arc::new(NoticeEnvironment::default());
    let model = Arc::new(ScriptedModel::new(Vec::new()));
    let session = super::notice::session(&scope, env.clone(), model, Vec::new(), Vec::new()).await;
    session.close(EndReason::PromptInputExit).await;
    assert_eq!(env.closed(), [(SessionId::new("sess_1"), true)]);
}

/// 等到被取消才结束的模型；记下被调用的次数，每次被调用时发一次 `called`。
struct HangingModel {
    calls: std::sync::atomic::AtomicUsize,
    called: tokio::sync::Notify,
}

impl ModelPort for HangingModel {
    fn stream(
        &self,
        _request: ModelRequest,
        cancel: CancellationToken,
    ) -> BoxStream<'static, Result<ModelEvent, ModelError>> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.called.notify_one();
        stream::once(async move {
            cancel.cancelled().await;
            Err(ModelError::Cancelled)
        })
        .boxed()
    }
}

fn user_texts(messages: &[rust_cli_kernel_api::message::Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| match message {
            rust_cli_kernel_api::message::Message::User(user) => {
                user.content.iter().find_map(|block| match block {
                    rust_cli_kernel_api::message::ContentBlock::Text { text } => Some(text.clone()),
                    _ => None,
                })
            }
            _ => None,
        })
        .collect()
}

/// 运行中关闭：中止回合、等它收尾，排队的输入不再开回合；之后 actor 退出。
#[tokio::test]
async fn closing_a_running_turn_interrupts_it_and_starts_nothing_queued() {
    let model = Arc::new(HangingModel {
        calls: std::sync::atomic::AtomicUsize::new(0),
        called: tokio::sync::Notify::new(),
    });
    let built = fixture(model.clone(), true).await;
    let session = &built.session;
    session.send(submit("c1", "first")).await;
    // 修复线 F35：等模型被调用（回合已提交输入、在等回复），不等 TurnStarted。TurnStarted 在 UserPromptSubmit 与
    // 提交输入之前发出，关闭落在这段空档里时回合在调用模型之前中止，`calls` 是 0。
    model.called.notified().await;
    session.send(submit("c2", "second")).await;
    session.close(EndReason::Clear).await;
    assert!(session.view().await.is_err());
    let stored = built.store.load(&SessionId::new("sess_1")).await.unwrap();
    let texts = user_texts(&stored.messages);
    assert!(
        !texts.iter().any(|text| text == "second"),
        "the queued input must not start a turn after close: {texts:?}"
    );
    assert_eq!(model.calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
