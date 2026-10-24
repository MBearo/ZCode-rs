//! `/fork` 按用户输入分叉（docs/specs/rust-cli/session-lifecycle.md §6.5，工作线 F75）：选择器列出带序号的输入，
//! 选中一条后切到新会话，新会话带到这一轮为止；文件不动；带参数时忽略参数（F78）；文字随界面语言（F78）；
//! `/branch` 是未知命令。
use crossterm::event::KeyCode;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::{ContentBlock, Message, UserOrigin};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::{environments, text_response};

use super::support::{GatedModel, Setup, with_ui};

fn idle_with(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

/// 人类输入与回答的文字（工具结果、attachment 不算）。
fn said(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter_map(|message| {
            let content = match message {
                Message::User(user) if user.origin == UserOrigin::Human => &user.content,
                Message::Assistant(assistant) => &assistant.content,
                _ => return None,
            };
            content.iter().find_map(|block| match block {
                ContentBlock::Text { text } => Some(text.clone()),
                _ => None,
            })
        })
        .collect()
}

/// 三轮之后 `/fork`：列表是带序号的三条输入、焦点在最新的一条；上移选第二条，切到新会话，新会话正好到第二轮的
/// 回答（第三轮不在）。带参数的旧写法（`latest`）忽略参数，照样打开选择器（F78；以前报错、不打开）。
#[tokio::test(flavor = "multi_thread")]
async fn fork_lists_the_inputs_and_keeps_the_chosen_turn() {
    let model = GatedModel::scripted(vec![
        text_response(&["one"]),
        text_response(&["two"]),
        text_response(&["three"]),
    ]);
    let setup = Setup::default();
    let store = setup.store.clone();
    let exit = with_ui(model, setup, |mut ui| async move {
        for (prompt, answer) in [("first", "one"), ("second", "two"), ("third", "three")] {
            ui.submit(prompt);
            let answer = format!("⏺ {answer}");
            ui.wait("the answer", |s| idle_with(s, &answer)).await;
        }
        ui.submit("/fork latest");
        let picker = ui
            .wait_for("Fork the conversation after one of your messages.")
            .await;
        for row in ["1. first", "2. second", "› 3. third"] {
            assert!(picker.contains(row), "{row}: {picker}");
        }
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Switched to forked session").await;
        assert!(screen.contains("⏺ two"), "{screen}");
        assert!(!screen.contains("three"), "{screen}");
    })
    .await;
    let child = SessionId::new(exit.expect("the forked session is open"));
    let loaded = store.load(&child).await.unwrap();
    assert_eq!(said(&loaded.messages), ["first", "one", "second", "two"]);
    assert!(
        matches!(loaded.messages.last(), Some(Message::Attachment(a)) if a.source == "fork"),
        "{:?}",
        loaded.messages
    );
}

/// 写过文件的一轮分叉之后文件不动（要回退文件用 `/rewind`）；只有一条输入时直接 enter。
#[tokio::test(flavor = "multi_thread")]
async fn fork_leaves_the_files_as_they_are() {
    use rust_cli_kernel_api::env::{EnvFacts, EnvPath, ReadFile};
    use rust_cli_kernel_api::machine::Files;
    use rust_cli_kernel_api::testing::{MemoryArtifacts, MemoryEnvironment, tool_use_response};
    use std::sync::Arc;

    let model = GatedModel::scripted(vec![
        tool_use_response(
            "toolu_1",
            "Write",
            &serde_json::json!({"file_path": "/Users/dev/project/notes.txt", "content": "new\n"}),
        ),
        text_response(&["written"]),
    ]);
    let env = Arc::new(MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    }));
    let setup = Setup {
        environments: environments(env.clone()),
        artifacts: Some(Arc::new(MemoryArtifacts::default())),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("write the notes");
        // 等权限询问本身（只有对话框里有这句），过了按键保护再允许这一次。
        ui.wait_for("Do you want to write").await;
        ui.settle().await;
        ui.key(KeyCode::Enter);
        ui.wait("the turn to end", |s| idle_with(s, "written"))
            .await;
        ui.submit("/fork");
        let picker = ui.wait_for("› 1. write the notes").await;
        assert!(!picker.contains("restore files"), "{picker}");
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Switched to forked session").await;
        assert!(screen.contains("copied"), "{screen}");
        assert!(screen.contains("written"), "{screen}");
    })
    .await;
    let workspace = rust_cli_kernel_api::env::WorkspaceRef {
        environment: rust_cli_kernel_api::ids::EnvironmentId::new("local"),
        root: EnvPath::new("/Users/dev/project"),
    };
    let read = env
        .read_file(
            &workspace,
            ReadFile {
                path: EnvPath::new("/Users/dev/project/notes.txt"),
            },
        )
        .await
        .expect("the written file is still there");
    assert_eq!(read.bytes, b"new\n");
}

/// `/branch` 已按用户决定删除：按未知命令提示，输入保留，不分叉。
#[tokio::test(flavor = "multi_thread")]
async fn branch_is_an_unknown_command() {
    let model = GatedModel::scripted(vec![text_response(&["hello back"])]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("hello");
        ui.wait("the answer", |s| idle_with(s, "hello back")).await;
        ui.submit("/branch");
        let screen = ui.wait_for("Unrecognized command '/branch'").await;
        assert!(screen.contains("❯ /branch"), "{screen}");
        assert!(!screen.contains("Branched conversation"), "{screen}");
    })
    .await;
}

/// 中文界面（工作线 F78）：选择器是中文；分叉后的回执与切换提示也是中文，不是 Node 的英文原文。
#[tokio::test(flavor = "multi_thread")]
async fn fork_texts_follow_the_interface_language() {
    let model = GatedModel::scripted(vec![text_response(&["one"])]);
    let setup = Setup {
        lang: "zh_CN.UTF-8",
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("first");
        ui.wait("the answer", |s| {
            s.contains("⏺ one") && !s.contains("esc 中断")
        })
        .await;
        ui.submit("/fork");
        let picker = ui.wait_for("从某条输入之后分出新会话。").await;
        assert!(picker.contains("› 1. first"), "{picker}");
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("已切换到分出的会话").await;
        assert!(screen.contains("分出会话"), "{screen}");
        assert!(screen.contains("条消息。"), "{screen}");
        assert!(!screen.contains("Switched to forked session"), "{screen}");
        assert!(!screen.contains("Forked session"), "{screen}");
    })
    .await;
}
