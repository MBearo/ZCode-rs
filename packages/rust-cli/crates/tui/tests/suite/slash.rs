// 改写自 apps/zcode-host/crates/tui/tests/suite/settings.rs 与 manage.rs（feat/rs 79b6691c55）的用意：斜杠
// 命令的核心子集（docs/specs/rust-cli/tui.md §6）。
use crossterm::event::KeyCode;
use rust_cli_kernel_api::side_request::SideSource;
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, with_ui};

/// 画面里有回复行 `   <text>`，且回合已结束（不再显示 “esc stop”）。
/// 回复从第 3 列起、没有记号（tui-kit.md §4.2「回答」）：整行是三个空格加文字。
fn replied(screen: &str, text: &str) -> bool {
    let line = format!("   {text}");
    screen.lines().any(|row| row.trim_end() == line) && !screen.contains("esc stop")
}

/// `/help` 是 `/keys` 的别名（Puffin）：草稿换成 `?`，板上是按键表；esc 关上、草稿留着。以前是一整页的命令与快捷键，
/// 没实现的命令标 `(not implemented)`。
#[tokio::test(flavor = "multi_thread")]
async fn help_opens_the_key_sheet() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/help");
        let sheet = ui.wait_for("Anywhere").await;
        assert!(sheet.contains("› ?"), "{sheet}");
        ui.key(KeyCode::Esc);
        ui.wait("the sheet to close", |s| !s.contains("Anywhere"))
            .await;
    })
    .await;
}

/// 现在不能运行的命令（Puffin 命令架的灰色斜体）：说原因，⏎ 什么都不做、草稿留着；不认识的命令照旧说明、留在输入框里。
/// 以前 `/btw` 说 `/btw is not implemented yet.`，没有会话时 `/rewind` 打开只有一句说明的列表。
#[tokio::test(flavor = "multi_thread")]
async fn unavailable_and_unknown_commands_only_explain() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/btw");
        let screen = ui.wait_for("Not available yet.").await;
        assert!(screen.contains("› /btw"), "{screen}");
        ui.clear();
        ui.submit("/rewind");
        let screen = ui.wait_for("Nothing to rewind yet.").await;
        assert!(screen.contains("› /rewind"), "{screen}");
        ui.key(KeyCode::Esc);
        ui.wait("the shelf to close", |s| {
            !s.contains("Nothing to rewind yet.") && s.contains("› /rewind")
        })
        .await;
        ui.clear();
        ui.submit("/nope");
        let screen = ui.wait_for("Unrecognized command '/nope'").await;
        // 不存在的命令留在输入框里。
        assert!(screen.contains("› /nope"), "{screen}");
    })
    .await;
    assert!(model.requests().is_empty(), "no command reached the model");
}

#[tokio::test(flavor = "multi_thread")]
async fn model_lists_the_directory_and_switches_the_session() {
    let model = GatedModel::scripted(vec![text_response(&["hi"]), text_response(&["again"])]);
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("hello");
        // 缺陷修复：原来等 `contains("hi")`，footer 的 “shift+tab” 也含 “hi”，回复没到、会话还没建好时条件就
        // 成立，下一条输入被忽略（偶发失败）。改成等回复行本身（`   hi`）且回合已结束。
        ui.wait("the first reply", |s| replied(s, "hi")).await;
        ui.submit("/model");
        // 选择器没有头一行（Puffin 的选择器）；开着时草稿是 `/model `。
        let list = ui.wait_for("other-model").await;
        assert!(list.contains("› /model"), "{list}");
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Set model to other-model").await;
        assert!(!screen.contains("› /model"), "{screen}");
        ui.submit("again please");
        // R5-10：原来等 `contains("again")`，用户自己的提示行 `❯ again please` 就满足，接着的“没有 esc to
        // interrupt”在回合开始前也成立，断言为空。改成等回复行本身且回合已结束。
        ui.wait("the second reply", |s| replied(s, "again")).await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests[0].selection.model, "scripted");
    assert_eq!(requests[1].selection.model, "other-model");
}

// shift+tab 与 `/plan` 见 modes.rs（修复线 F20）。

#[tokio::test(flavor = "multi_thread")]
async fn clear_leaves_the_session_and_copy_takes_the_last_reply() {
    let model = GatedModel::scripted(vec![
        text_response(&["the answer"]),
        text_response(&["fresh"]),
    ]);
    let session = with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("question");
        ui.wait("the reply", |s| {
            s.contains("the answer") && !s.contains("esc stop")
        })
        .await;
        ui.submit("/copy");
        ui.wait_for("Copied the last reply").await;
        assert_eq!(*ui.copies.lock().unwrap(), ["the answer"]);
        ui.submit("/clear");
        let screen = ui.wait_for("Started a new session.").await;
        assert!(!screen.contains("the answer"), "{screen}");
        ui.submit("next question");
        ui.wait_for("fresh").await;
    })
    .await;
    // 第二个会话是新建的：它的请求里没有第一个会话的内容。
    let requests = model.requests();
    assert!(!super::support::request_text(&requests[1]).contains("the answer"));
    assert!(session.is_some());
}

/// 不带参数的 `/rename`（titles.md §8.6）：模型起名，显示结果；没有对话时显示说明。
#[tokio::test(flavor = "multi_thread")]
async fn rename_without_a_name_asks_the_model() {
    let model = GatedModel::scripted(vec![
        text_response(&["hi"]),
        text_response(&["{\"name\":\"hello-session\"}"]),
    ]);
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/rename");
        // 还没有会话：没有对话可起名（原文）。
        ui.wait_for("Could not generate a name: no conversation context yet.")
            .await;
        ui.submit("hello");
        // 缺陷修复：原来等 `contains("hi")`，footer 的 “shift+tab” 也含 “hi”，回复没到、会话还没建好时条件就
        // 成立，下一条输入被忽略（偶发失败）。改成等回复行本身（`   hi`）且回合已结束。
        ui.wait("the first reply", |s| replied(s, "hi")).await;
        ui.submit("/name");
        ui.wait_for("Session renamed to: hello-session").await;
        // `/rename ` 的 ghost 是现在的名字，⇥ 把它接进草稿（Puffin `rename_ghost`）。
        ui.type_text("/rename ");
        ui.wait("the ghost", |s| s.contains("› /rename hello-session"))
            .await;
        ui.key(KeyCode::Tab);
        ui.type_text("-2");
        ui.wait_for("› /rename hello-session-2").await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].source,
        rust_cli_kernel_api::model::QuerySource::Side(SideSource::Rename)
    );
}
