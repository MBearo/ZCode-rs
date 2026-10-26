// 改写自 apps/zcode-host/crates/tui/tests/suite/settings.rs 与 manage.rs（feat/rs 79b6691c55）的用意：斜杠
// 命令的核心子集（docs/specs/rust-cli/tui.md §6）。
use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, with_ui};

/// 画面里有回复行 `⏺ <text>`，且回合已结束（不再显示 “esc to interrupt”）。
fn replied(screen: &str, text: &str) -> bool {
    let line = format!("\u{23fa} {text}");
    screen.lines().any(|row| row.trim() == line) && !screen.contains("esc to interrupt")
}

#[tokio::test(flavor = "multi_thread")]
async fn help_lists_the_commands_and_marks_the_missing_ones() {
    let model = GatedModel::scripted(Vec::new());
    let setup = Setup {
        width: 140,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/help");
        let page = ui.wait_for("Slash commands").await;
        assert!(page.contains("/clear"), "{page}");
        // 别名列在说明后面（`/clear` 的说明较长，别名在 80 列里被截掉，看 `/rename`）。
        assert!(
            page.contains("Rename the current conversation  /name"),
            "{page}"
        );
        // L27 实现了 `/memory`、L29 实现了 `/diff` 等：剩下的 `/btw` 说明较长，往下滚并看末尾的标记（修复线 L-A 起
        // `/init` 是内核给出的 skill 命令）。
        for _ in 0..12 {
            ui.key(KeyCode::Down);
            if ui.screen().contains("(not implemented)") {
                break;
            }
        }
        ui.wait_for("(not implemented)").await;
        ui.key(KeyCode::Esc);
        ui.wait("the page to close", |s| !s.contains("Slash commands"))
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_and_unknown_commands_only_explain() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/btw");
        ui.wait_for("/btw is not implemented yet.").await;
        // 没有会话时 rewind 选择器只说明没有可回退的消息（原文）。
        ui.submit("/rewind");
        ui.wait_for("Nothing to rewind to yet.").await;
        ui.key(crossterm::event::KeyCode::Esc);
        ui.submit("/nope");
        let screen = ui.wait_for("Unrecognized command '/nope'").await;
        // 不存在的命令留在输入框里。
        assert!(screen.contains("❯ /nope"), "{screen}");
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
        // 成立，下一条输入被忽略（偶发失败）。改成等回复行本身（`⏺ hi`）且回合已结束。
        ui.wait("the first reply", |s| replied(s, "hi")).await;
        ui.submit("/model");
        let list = ui.wait_for("Select model").await;
        assert!(list.contains("other-model"), "{list}");
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Set model to other-model").await;
        assert!(!screen.contains("Select model"), "{screen}");
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
            s.contains("the answer") && !s.contains("esc to interrupt")
        })
        .await;
        ui.submit("/copy");
        ui.wait_for("Copied the last reply").await;
        assert_eq!(*ui.copies.lock().unwrap(), ["the answer"]);
        ui.submit("/clear");
        let screen = ui.wait_for("Started a new session.").await;
        assert!(!screen.contains("the answer"), "{screen}");
        ui.submit("/status");
        let status = ui.wait_for("Session:").await;
        assert!(status.contains("not started"), "{status}");
        ui.key(KeyCode::Esc);
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
        // 成立，下一条输入被忽略（偶发失败）。改成等回复行本身（`⏺ hi`）且回合已结束。
        ui.wait("the first reply", |s| replied(s, "hi")).await;
        ui.submit("/name");
        ui.wait_for("Session renamed to: hello-session").await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(
        requests[1].source,
        rust_cli_kernel_api::model::QuerySource::Side("rename_generate_name".to_owned())
    );
}
