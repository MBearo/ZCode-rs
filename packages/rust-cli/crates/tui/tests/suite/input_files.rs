//! `@` 文件补全与提及（docs/specs/rust-cli/tui-input.md §1）：输入 `@` 时列出工作区文件、模糊匹配、tab 先补
//! 公共前缀、enter 接受；发送后模型收到 Read 的调用说明与文件内容。`@` 图片时模型收到 image 块（§1.3）。

use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{MemoryEnvironment, text_response};

use super::support::{GatedModel, Setup, request_text, with_ui};

fn setup() -> Setup {
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new("/Users/dev")),
    })
    .with_file("/Users/dev/project/src/main.rs", "fn main() {}\n")
    .with_file("/Users/dev/project/src/lib.rs", "pub fn lib() {}\n")
    .with_file("/Users/dev/project/README.md", "# Project\n")
    .with_file("/Users/dev/project/shot.png", "png bytes");
    Setup {
        env: Arc::new(env),
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn at_completes_workspace_files_and_the_model_sees_the_file() {
    let model = GatedModel::scripted(vec![text_response(&["read it"])]);
    with_ui(model.clone(), setup(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // 只输入 `@`：顶层条目（目录带分隔符）。
        ui.type_text("look at @");
        ui.wait("top level entries", |s| {
            s.contains("src/") && s.contains("README.md")
        })
        .await;
        ui.type_text("mai");
        ui.wait_for("src/main.rs").await;
        // tab：唯一候选比已输入的长，先补到公共前缀（不加空格）；enter 接受并加空格。
        ui.key(KeyCode::Tab);
        ui.wait_for("look at @src/main.rs").await;
        ui.key(KeyCode::Enter);
        ui.type_text("please");
        ui.wait_for("@src/main.rs please").await;
        ui.key(KeyCode::Enter);
        ui.wait("the reply", |s| {
            s.contains("read it") && !s.contains("esc to interrupt")
        })
        .await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    let text = request_text(&requests[0]);
    assert!(
        text.contains(
            "Called the Read tool with the following input: {\\\"file_path\\\":\\\"/Users/dev/project/src/main.rs\\\"}"
        ),
        "{text}"
    );
    assert!(text.contains("1\\tfn main() {}"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_closes_the_file_list_and_unknown_queries_show_nothing() {
    let model = GatedModel::scripted(vec![]);
    with_ui(model, setup(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("@lib");
        ui.wait_for("src/lib.rs").await;
        ui.key(KeyCode::Esc);
        ui.wait("the list closed", |s| !s.contains("src/lib.rs"))
            .await;
        ui.type_text("zzz");
        let screen = ui.wait_for("@libzzz").await;
        assert!(!screen.contains("src/"), "{screen}");
    })
    .await;
}

/// `@` 图片（tui-input.md §1.3）：调用说明单独一条 user 消息，image 块另起一条，人类输入接在后面。
#[tokio::test(flavor = "multi_thread")]
async fn a_mentioned_image_reaches_the_model_as_an_image_block() {
    use rust_cli_kernel_api::message::{ContentBlock, ImageSource};

    let model = GatedModel::scripted(vec![text_response(&["a picture"])]);
    with_ui(model.clone(), setup(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("what is @shot.png now");
        ui.wait_for("@shot.png now").await;
        ui.key(KeyCode::Enter);
        ui.wait("the reply", |s| {
            s.contains("a picture") && !s.contains("esc to interrupt")
        })
        .await;
    })
    .await;
    let requests = model.requests();
    let messages = &requests[0].messages;
    let call = "<system-reminder>\nCalled the Read tool with the following input: {\"file_path\":\"/Users/dev/project/shot.png\"}\n</system-reminder>";
    assert_eq!(
        messages[0].content[0].block,
        ContentBlock::Text {
            text: call.to_owned()
        }
    );
    // MemoryEnvironment 的文件内容不是真的 PNG：类型按 Read 的缺省（image/png），数据是原样的 base64。
    assert_eq!(
        messages[1].content[0].block,
        ContentBlock::Image {
            source: ImageSource::Base64 {
                media_type: "image/png".to_owned(),
                data: "cG5nIGJ5dGVz".to_owned(),
            }
        }
    );
    let last = messages[1].content.last().map(|item| &item.block);
    assert_eq!(
        last,
        Some(&ContentBlock::Text {
            text: "what is @shot.png now".to_owned()
        })
    );
}
