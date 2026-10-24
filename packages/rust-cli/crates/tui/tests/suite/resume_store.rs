//! 续写时补的 `No response requested.` 什么时候落库（docs/specs/rust-cli/session-lifecycle.md §6.3.1，工作线 F79）：
//! 会话里 `/resume` 切过去就写；启动时打开（`--resume <id>`、启动时的 `/resume` 页）等第一处改动再写；来回切换只写一次。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::store::Fact;
use rust_cli_kernel_api::testing::{MemoryStore, text_response};

use super::sessions_seed::{Seed, seed};
use super::support::{GatedModel, Setup, with_ui};

/// 被中断的会话：只有一条没有回答的输入（`reply` 为空时不放回复）。
const INTERRUPTED: Seed = Seed {
    id: "sess_cut",
    root: "/Users/dev/project",
    title: "Cut short",
    prompt: "unanswered question",
    reply: "",
    created: 1_000_000,
    updated: 2_000_000,
};

/// 另一个答完的会话（来回切换用）。
const ANSWERED: Seed = Seed {
    id: "sess_done",
    root: "/Users/dev/project",
    title: "Done work",
    prompt: "finished question",
    reply: "finished reply",
    created: 500_000,
    updated: 1_000_000,
};

async fn setup() -> (Arc<MemoryStore>, Setup) {
    let store = Arc::new(MemoryStore::default());
    seed(&store, &[INTERRUPTED, ANSWERED]).await;
    let setup = Setup {
        width: 100,
        height: 30,
        store: store.clone(),
        ..Setup::default()
    };
    (store, setup)
}

/// 库里这个会话的消息：续写占位记作 `S`，其余是消息 ID。
fn stored(store: &MemoryStore, id: &str) -> Vec<String> {
    store
        .facts(&SessionId::new(id))
        .iter()
        .filter_map(|fact| match fact {
            Fact::MessageAppended {
                message: Message::Assistant(assistant),
            } if assistant.is_resume_sentinel() => Some("S".to_owned()),
            Fact::MessageAppended { message } => Some(message.id().as_str().to_owned()),
            _ => None,
        })
        .collect()
}

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text)
        && !screen.contains("esc to interrupt")
        && !screen.contains("Type to search")
}

#[tokio::test(flavor = "multi_thread")]
async fn resuming_inside_the_session_stores_the_reply_at_once() {
    let (store, setup) = setup().await;
    let check = store.clone();
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.submit("/resume sess_cut");
            ui.wait("the interrupted session", |s| {
                idle(s, "unanswered question")
            })
            .await;
            // 切过去就写，不等下一条输入。
            assert_eq!(stored(&check, "sess_cut"), ["msg_u_sess_cut", "S"]);
        },
    )
    .await;
    assert_eq!(stored(&store, "sess_cut"), ["msg_u_sess_cut", "S"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn opening_at_startup_waits_for_the_first_input() {
    let (store, mut setup) = setup().await;
    setup.startup = rust_cli_tui::Startup::Session("sess_cut".to_owned());
    let check = store.clone();
    let model = GatedModel::scripted(vec![text_response(&["next reply"])]);
    with_ui(model, setup, |mut ui| async move {
        ui.wait("the interrupted session", |s| {
            idle(s, "unanswered question")
        })
        .await;
        ui.settle().await;
        assert_eq!(stored(&check, "sess_cut"), ["msg_u_sess_cut"]);
        ui.submit("next question");
        ui.wait("the reply", |s| idle(s, "next reply")).await;
        // 占位随新输入那一批写，排在新的消息之前，只有一条。
        let after = stored(&check, "sess_cut");
        assert_eq!(after[..2], ["msg_u_sess_cut", "S"], "{after:?}");
        assert!(after.len() > 2, "{after:?}");
        assert_eq!(after.iter().filter(|id| *id == "S").count(), 1, "{after:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_startup_page_waits_for_the_first_input() {
    let (store, mut setup) = setup().await;
    setup.startup = rust_cli_tui::Startup::Pick;
    let check = store.clone();
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            // 被中断的会话活动时间更近，排在第一行（列表显示标题）。
            ui.wait("the session list", |s| {
                s.contains("Type to search") && s.contains("› ") && s.contains("Cut short")
            })
            .await;
            ui.key(KeyCode::Enter);
            ui.wait("the interrupted session", |s| {
                idle(s, "unanswered question")
            })
            .await;
            ui.settle().await;
            assert_eq!(stored(&check, "sess_cut"), ["msg_u_sess_cut"]);
        },
    )
    .await;
    assert_eq!(stored(&store, "sess_cut"), ["msg_u_sess_cut"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_back_and_forth_stores_one_reply() {
    let (store, setup) = setup().await;
    let check = store.clone();
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.submit("/resume sess_cut");
            ui.wait("the interrupted session", |s| {
                idle(s, "unanswered question")
            })
            .await;
            ui.submit("/resume sess_done");
            ui.wait("the answered session", |s| {
                idle(s, "finished reply") && !s.contains("unanswered question")
            })
            .await;
            ui.submit("/resume sess_cut");
            ui.wait("the interrupted session again", |s| {
                idle(s, "unanswered question") && !s.contains("finished reply")
            })
            .await;
            // 第二次打开时它已是最后一条（assistant），不再补。
            assert_eq!(stored(&check, "sess_cut"), ["msg_u_sess_cut", "S"]);
            assert_eq!(
                stored(&check, "sess_done"),
                ["msg_u_sess_done", "msg_a_sess_done"]
            );
        },
    )
    .await;
}
