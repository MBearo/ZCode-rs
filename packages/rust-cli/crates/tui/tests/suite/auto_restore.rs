//! esc 撤回刚发的输入（修复线 F38，docs/specs/rust-cli/tui-input.md §8、session-lifecycle.md §5.5）：
//! 用户中断的回合结束时，这条输入还没得到回答、输入框为空、没有排队的输入，就把对话回退到它
//! 之前、文字放回输入框；回答过（流出了文字）或输入框有草稿时保留输入与中断标记。运行中的 `/clear` 等回合结束再执行
//! （排进队列、不中断）。模型的响应由测试控制节奏；“模型已被调用”按模型记下的请求判断（输入已提交、在等回复）。
//! 按键之前另等画面上的回合（footer 的 `esc to interrupt`）：esc、运行中的 `/clear` 与插话按画面解释，模型收到请求时
//! 画面可能还停在空闲（tui.md §4.3、§8，修复线 F45；那个窗口里的 esc 见 `esc_windows.rs`）。
use std::sync::Arc;
use std::time::Duration;

use crossterm::event::{KeyCode, KeyModifiers};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::ContentBlock;
use rust_cli_kernel_api::model::{BlockDelta, ModelEvent};
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::testing::{MemoryStore, text_response};
use rust_cli_tui::{Clipboard, ImageBytes, Services};

use super::support::{GatedModel, Item, Setup, finish, request_text, send, with_ui};

const MARKER: &str = "[Request interrupted by user]";
const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000IHDR";

/// 存储里的会话（内存存储不看筛选条件）。
async fn sessions(store: &MemoryStore) -> Vec<SessionId> {
    use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
    use rust_cli_kernel_api::ids::EnvironmentId;
    use rust_cli_kernel_api::store::{SessionFilter, SessionStore};
    let filter = SessionFilter {
        workspace: WorkspaceRef {
            environment: EnvironmentId::new("local"),
            root: EnvPath::new("/Users/dev/project"),
        },
        limit: 10,
    };
    store
        .list(filter)
        .await
        .unwrap()
        .into_iter()
        .map(|summary| summary.id)
        .collect()
}

/// 等模型收到第 `n` 个请求（输入已提交、主循环在等回复）；期限只为让失败的测试结束。
async fn called(model: &GatedModel, n: usize) {
    let wait = async {
        while model.requests().len() < n {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    };
    if tokio::time::timeout(Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!("timed out waiting for model request {n}");
    }
}

fn start() -> Item {
    Ok(ModelEvent::MessageStart {
        id: "msg_api".to_owned(),
        model: "scripted".to_owned(),
        usage: Default::default(),
    })
}

/// 撤回完成：输入框里是原文、对话里没有这一轮（没有中断的提示）、回合不在跑。
fn restored(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("Interrupted") && !screen.contains("esc to interrupt")
}

/// 请求已发、还没有第一个字时 esc：输入回到输入框（对话里没有这一轮），回车再发一次，模型只收到这条输入一次、
/// 没有中断标记；库里是 `RevertSet`（Node 的 `session.revert`）。旧代码：输入与中断标记留在对话里，输入框为空，
/// 回车不发任何东西。
#[tokio::test(flavor = "multi_thread")]
async fn esc_before_the_first_word_puts_the_input_back() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    let store = Arc::new(MemoryStore::default());
    let setup = Setup {
        store: store.clone(),
        ..Setup::default()
    };
    let waiting = model.clone();
    let session = with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("first prompt");
        called(&waiting, 1).await;
        ui.wait_for("esc to interrupt").await;
        send(&first, vec![start()]);
        ui.key(KeyCode::Esc);
        ui.wait("the input back in the box", |s| restored(s, "first prompt"))
            .await;
        finish(second, text_response(&["again"]));
        ui.key(KeyCode::Enter);
        ui.wait("the reply", |s| {
            s.contains("again") && !s.contains("esc to interrupt")
        })
        .await;
        drop(first);
    })
    .await
    .expect("a session was created");
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
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

/// 模型流出了文字之后 esc：回答过，不撤回——输入、半句与中断标记留在对话里（半句也进下一条请求，query.md §1.4）。
/// 旧代码丢掉没写完的半句，下一条请求里没有它。
#[tokio::test(flavor = "multi_thread")]
async fn esc_after_some_text_keeps_the_input_with_the_marker() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("first prompt");
        send(
            &first,
            vec![
                start(),
                Ok(ModelEvent::BlockStart {
                    index: 0,
                    block: ContentBlock::Text {
                        text: String::new(),
                    },
                }),
                // 带换行：流式中没写完的最后一行不显示。
                Ok(ModelEvent::BlockDelta {
                    index: 0,
                    delta: BlockDelta::Text("partial reply\n".to_owned()),
                }),
            ],
        );
        ui.wait_for("partial reply").await;
        ui.key(KeyCode::Esc);
        let screen = ui.wait_for("Interrupted").await;
        assert!(screen.contains("first prompt"), "{screen}");
        finish(second, text_response(&["next reply"]));
        ui.submit("next prompt");
        ui.wait("the reply", |s| {
            s.contains("next reply") && !s.contains("esc to interrupt")
        })
        .await;
        drop(first);
    })
    .await;
    let requests = model.requests();
    let text = request_text(&requests[1]);
    let at = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
    let order = [
        at("first prompt"),
        at("partial reply"),
        at(MARKER),
        at("next prompt"),
    ];
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]) && order[3] != usize::MAX,
        "{text}"
    );
}

/// 输入框里有草稿时 esc：不撤回（要输入框为空），对话里是输入与中断的提示，草稿留在输入框。
#[tokio::test(flavor = "multi_thread")]
async fn esc_with_a_draft_keeps_the_input_with_the_marker() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    let waiting = model.clone();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("first prompt");
        called(&waiting, 1).await;
        ui.type_text("my draft");
        ui.wait("the draft on a running turn", |s| {
            s.contains("my draft") && s.contains("esc to interrupt")
        })
        .await;
        ui.key(KeyCode::Esc);
        ui.wait_for("Interrupted").await;
        finish(second, text_response(&["draft reply"]));
        ui.key(KeyCode::Enter);
        ui.wait("the reply", |s| {
            s.contains("draft reply") && !s.contains("esc to interrupt")
        })
        .await;
        drop(first);
    })
    .await;
    let text = request_text(&model.requests()[1]);
    let at = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
    let order = [at("first prompt"), at(MARKER), at("my draft")];
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]) && order[2] != usize::MAX,
        "{text}"
    );
}

/// 有排队的输入时 esc：不撤回，排队的输入接着作为新回合发出，带着第一条输入与中断标记。
#[tokio::test(flavor = "multi_thread")]
async fn esc_with_a_queued_input_keeps_the_input() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    let waiting = model.clone();
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("first prompt");
        called(&waiting, 1).await;
        ui.wait_for("esc to interrupt").await;
        ui.submit("queued prompt");
        ui.wait_for("❯ queued prompt").await;
        finish(second, text_response(&["queued reply"]));
        ui.key(KeyCode::Esc);
        ui.wait("the reply", |s| {
            s.contains("queued reply") && !s.contains("esc to interrupt")
        })
        .await;
        drop(first);
    })
    .await;
    let text = request_text(&model.requests()[1]);
    let at = |needle: &str| text.find(needle).unwrap_or(usize::MAX);
    let order = [at("first prompt"), at(MARKER), at("queued prompt")];
    assert!(
        order.windows(2).all(|pair| pair[0] < pair[1]) && order[2] != usize::MAX,
        "{text}"
    );
}

/// 剪贴板替身：`image` 给出一张 PNG。
struct FakeClipboard;

impl Clipboard for FakeClipboard {
    fn image(&self) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        async move {
            Ok(Some(ImageBytes {
                bytes: PNG.to_vec(),
            }))
        }
        .boxed()
    }

    fn image_file(&self, _path: String) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        async move { Ok(None) }.boxed()
    }
}

/// 带图片的输入：`[Image #1]` 与图片一起回到输入框，再发送时模型照样收到 image 块。
#[tokio::test(flavor = "multi_thread")]
async fn esc_puts_an_image_input_back_with_its_image() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    let setup = Setup {
        services: Services {
            clipboard: Some(Arc::new(FakeClipboard)),
            ..Services::default()
        },
        ..Setup::default()
    };
    let waiting = model.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("look at ");
        let paste = if cfg!(windows) {
            KeyModifiers::ALT
        } else {
            KeyModifiers::CONTROL
        };
        ui.press(KeyCode::Char('v'), paste);
        ui.wait_for("look at [Image #1]").await;
        ui.submit(" please");
        called(&waiting, 1).await;
        ui.wait_for("esc to interrupt").await;
        ui.key(KeyCode::Esc);
        ui.wait("the input back in the box", |s| {
            restored(s, "look at [Image #1] please")
        })
        .await;
        finish(second, text_response(&["a tiny png"]));
        ui.key(KeyCode::Enter);
        ui.wait("the reply", |s| {
            s.contains("a tiny png") && !s.contains("esc to interrupt")
        })
        .await;
        drop(first);
    })
    .await;
    let requests = model.requests();
    let text = request_text(&requests[1]);
    assert_eq!(
        text.matches("look at [Image #1] please").count(),
        1,
        "{text}"
    );
    assert_eq!(text.matches("\"type\":\"image\"").count(), 1, "{text}");
}

/// 运行中的 `/clear`：不中断（排进队列），显示在输入框上方；回合照常跑完（没有中断标记）之后才清掉对话。
/// 旧代码先中断再离开：库里是输入与中断标记，没有回复。
#[tokio::test(flavor = "multi_thread")]
async fn clear_while_running_waits_for_the_turn() {
    let (model, senders) = GatedModel::new(1);
    let first = senders.into_iter().next().unwrap();
    let store = Arc::new(MemoryStore::default());
    let setup = Setup {
        store: store.clone(),
        ..Setup::default()
    };
    let waiting = model.clone();
    let observed = store.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("first prompt");
        called(&waiting, 1).await;
        // `/clear` 排不排到回合之后看画面上的回合（tui.md §4.3）。
        ui.wait_for("esc to interrupt").await;
        ui.submit("/clear");
        let screen = ui.wait_for("❯ /clear").await;
        assert!(screen.contains("esc to interrupt"), "{screen}");
        finish(first, text_response(&["whole reply"]));
        ui.wait_for("Started a new session.").await;
        assert_eq!(sessions(&observed).await.len(), 1);
    })
    .await;
    let session = sessions(&store).await.remove(0);
    let facts = format!("{:?}", store.facts(&session));
    assert!(facts.contains("whole reply"), "{facts}");
    assert!(!facts.contains(MARKER), "{facts}");
}

/// 运行中先排了一条输入、再 `/clear`：按排队的顺序，排着的输入作为下一个回合跑完之后才清掉对话（队列按顺序处理）。
/// 内核在回合结束之后才取出排着的输入：先发 `QueueChanged`（队列空了）、再发下一个回合的 `TurnStarted`。旧代码每个
/// 事件之后都看“空闲且队列空”，在这两个事件之间就执行 `/clear`，关闭会话时以用户中断中止刚开始的回合：画面上不会
/// 出现这个回合的回复（修复线 F45）。
#[tokio::test(flavor = "multi_thread")]
async fn clear_after_a_queued_input_waits_for_its_turn() {
    let (model, senders) = GatedModel::new(2);
    let mut senders = senders.into_iter();
    let (first, second) = (senders.next().unwrap(), senders.next().unwrap());
    let store = Arc::new(MemoryStore::default());
    let setup = Setup {
        store: store.clone(),
        ..Setup::default()
    };
    let waiting = model.clone();
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("first prompt");
        called(&waiting, 1).await;
        ui.wait_for("esc to interrupt").await;
        ui.submit("queued prompt");
        ui.wait_for("❯ queued prompt").await;
        ui.submit("/clear");
        ui.wait_for("❯ /clear").await;
        finish(first, text_response(&["whole reply"]));
        called(&waiting, 2).await;
        let mut reply = text_response(&["queued reply\n"]);
        let rest = reply.split_off(3);
        send(&second, reply);
        // 排着的输入在跑自己的回合，`/clear` 还在等。
        let screen = ui.wait_for("queued reply").await;
        assert!(screen.contains("❯ /clear"), "{screen}");
        finish(second, rest);
        ui.wait_for("Started a new session.").await;
    })
    .await;
    assert_eq!(model.requests().len(), 2, "the queued input had its turn");
    let session = sessions(&store).await.remove(0);
    let facts = format!("{:?}", store.facts(&session));
    assert!(facts.contains("queued reply"), "{facts}");
    assert!(!facts.contains(MARKER), "{facts}");
}
