//! 画面与内核不一致的两个窗口里按 esc（修复线 F45，docs/specs/rust-cli/tui-input.md §8、tui.md §4.3）。视图跟在
//! 内核后面，按键按视图解释，命令由 actor 按它自己的事实处理。测试让 actor 停在一次提交里，把窗口撑开：
//! - 按 enter 之后、回合画出来之前 esc：走空闲的路径，发的是同一个中断，actor 中止刚开始的回合；照样撤回；
//! - 回合已经结束、画面还在运行时 esc：中断被 actor 拒绝（没有回合在跑），不提示。
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::store::{
    CommitReceipt, Fact, FactBatch, LoadedSession, SessionFilter, SessionMeta, SessionStore,
    SessionSummary, StoreError,
};
use rust_cli_kernel_api::testing::{MemoryStore, text_response};
use tokio::sync::Notify;

use super::support::{GatedModel, Setup, request_text, with_ui};

const MARKER: &str = "[Request interrupted by user]";

/// 回合不在跑（画面上没有中断提示）时画面里有 `text`。
fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

/// 停住哪一次提交：这一批里有满足条件的事实。
type Stop = fn(&Fact) -> bool;

/// 提交可以停住的存储：`hold` 之后第一次满足条件的提交先通知 `held`，等 `release` 再写。actor 停在这次提交里，
/// 之后的命令在它的信箱里按顺序排着，事件也不再发出。
struct HeldCommit {
    inner: Arc<MemoryStore>,
    stop: Mutex<Option<Stop>>,
    held: Notify,
    release: Notify,
}

impl HeldCommit {
    fn new(inner: Arc<MemoryStore>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            stop: Mutex::new(None),
            held: Notify::new(),
            release: Notify::new(),
        })
    }

    fn hold(&self, stop: Stop) {
        *self.stop.lock().unwrap() = Some(stop);
    }

    /// 等 actor 停在那次提交里；期限只为让失败的测试结束。
    async fn held(&self) {
        if tokio::time::timeout(Duration::from_secs(10), self.held.notified())
            .await
            .is_err()
        {
            panic!("timed out waiting for the held commit");
        }
    }

    fn release(&self) {
        self.release.notify_one();
    }
}

#[async_trait::async_trait]
impl SessionStore for HeldCommit {
    async fn create(&self, meta: SessionMeta) -> Result<(), StoreError> {
        self.inner.create(meta).await
    }

    async fn commit(
        &self,
        session: &SessionId,
        batch: FactBatch,
    ) -> Result<CommitReceipt, StoreError> {
        let stop = {
            let mut stop = self.stop.lock().unwrap();
            let hit = stop.is_some_and(|stop| batch.facts.iter().any(stop));
            if hit {
                *stop = None;
            }
            hit
        };
        if stop {
            self.held.notify_one();
            self.release.notified().await;
        }
        self.inner.commit(session, batch).await
    }

    async fn load(&self, session: &SessionId) -> Result<LoadedSession, StoreError> {
        self.inner.load(session).await
    }

    async fn list(&self, filter: SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
        self.inner.list(filter).await
    }
}

fn setup(store: &Arc<MemoryStore>, held: &Arc<HeldCommit>) -> Setup {
    Setup {
        store: store.clone(),
        session_store: Some(held.clone()),
        ..Setup::default()
    }
}

/// 按 enter 之后、回合画出来之前 esc（提交时同步进入 loading，这时的 esc 是 `user-cancel`）：画面还是空闲，esc 走
/// 取消唤醒的路径，发的同一个中断让 actor 中止刚开始的回合；回合结束后照样撤回到输入框。actor 先停在一次提交里
/// （shift+tab 改权限模式），输入与中断在它的信箱里排着，esc 一定在回合画出来之前处理。旧代码：只有运行中的路径
/// 记下用户中断，回合被中止了却不撤回，对话里留下输入与中断标记。
#[tokio::test(flavor = "multi_thread")]
async fn esc_before_the_turn_is_drawn_puts_the_input_back() {
    let model = GatedModel::scripted(vec![
        text_response(&["hello"]),
        text_response(&["second reply"]),
    ]);
    let store = Arc::new(MemoryStore::default());
    let held = HeldCommit::new(store.clone());
    let actor = held.clone();
    let session = with_ui(model.clone(), setup(&store, &held), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("hi");
        ui.wait("the first reply", |s| idle(s, "hello")).await;
        actor.hold(|fact| matches!(fact, Fact::ExecutionChanged { .. }));
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        actor.held().await;
        ui.submit("first prompt");
        ui.key(KeyCode::Esc);
        // esc 按空闲处理了（连按两次编辑的提示）：这时画面上还没有这个回合。
        let screen = ui.wait_for("Esc again to edit previous message").await;
        assert!(!screen.contains("esc to interrupt"), "{screen}");
        actor.release();
        ui.wait("the input back in the box", |s| {
            s.contains("first prompt") && idle(s, "hello") && !s.contains("Interrupted")
        })
        .await;
        ui.key(KeyCode::Enter);
        ui.wait("the reply", |s| idle(s, "second reply")).await;
    })
    .await
    .expect("a session was created");
    let requests = model.requests();
    assert_eq!(requests.len(), 2, "the interrupted turn sent no request");
    let text = request_text(&requests[1]);
    assert_eq!(text.matches("first prompt").count(), 1, "{text}");
    assert!(!text.contains(MARKER), "{text}");
    let facts = store.facts(&SessionId::new(session));
    assert!(
        facts
            .iter()
            .any(|fact| matches!(fact, Fact::RevertSet { .. })),
        "{facts:?}"
    );
}

/// 内核已经结束回合、`TurnFinished` 还没画出来时 esc：画面还是运行中，esc 发出中断；actor 这时没有回合在跑，拒绝它，
/// 不提示（同空闲路径的回执，没有这类提示）。actor 停在回合收尾的提交（用量行
/// `TurnEnded`）里，`TurnFinished` 在它之后发出。之后的 `/rename` 排在中断后面：它的回执画出来时，中断的回执已经
/// 处理过。旧代码：显示 `Not done: no turn is running`。
#[tokio::test(flavor = "multi_thread")]
async fn esc_after_the_turn_ended_reports_nothing() {
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    let store = Arc::new(MemoryStore::default());
    let held = HeldCommit::new(store.clone());
    let actor = held.clone();
    with_ui(model, setup(&store, &held), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        actor.hold(|fact| matches!(fact, Fact::TurnEnded { .. }));
        ui.submit("hi");
        actor.held().await;
        ui.wait("the reply of a running turn", |s| {
            s.contains("hello") && s.contains("esc to interrupt")
        })
        .await;
        ui.key(KeyCode::Esc);
        // 按键按顺序处理：输入框里出现这行字时，esc 已经按运行中处理了。
        ui.type_text("/rename later");
        ui.wait_for("❯ /rename later").await;
        actor.release();
        ui.wait("the turn to end", |s| idle(s, "hello")).await;
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Session renamed to: later").await;
        assert!(!screen.contains("Not done"), "{screen}");
    })
    .await;
}
