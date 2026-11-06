//! `@` 文件补全与提及（docs/specs/rust-cli/tui-input.md §1）：输入 `@` 时照 Puffin 的 `@` 面板列出工作区文件、按词排序，
//! ⇥ 或 ⏎ 放进选中的路径；发送后模型收到 Read 的调用说明与文件内容。`@` 图片时模型收到 image 块（§1.3）。

use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::env::{EnvFacts, EnvPath};
use rust_cli_kernel_api::testing::{MemoryEnvironment, environments, text_response};

use super::support::{GatedModel, Setup, request_text, with_ui};

fn setup() -> Setup {
    let env = MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/Users/dev/project"),
        platform: "darwin".into(),
        os_version: "x".into(),
        shell: "zsh".into(),
        is_git_repo: false,
        home: Some(EnvPath::new("/Users/dev")),
        uid: None,
    })
    .with_file("/Users/dev/project/src/main.rs", "fn main() {}\n")
    .with_file("/Users/dev/project/src/lib.rs", "pub fn lib() {}\n")
    .with_file("/Users/dev/project/README.md", "# Project\n")
    .with_file("/Users/dev/project/shot.png", "png bytes");
    Setup {
        environments: environments(Arc::new(env)),
        ..Setup::default()
    }
}

/// `@` 面板里有一行：名字，后面是它所在的文件夹（宽的终端在同一行的右边还有预览）。
fn shows_file(screen: &str, name: &str, folder: &str) -> bool {
    screen.lines().any(|line| {
        let words: Vec<&str> = line.split_whitespace().collect();
        words.len() >= 2 && words[..2] == [name, folder]
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn at_completes_workspace_files_and_the_model_sees_the_file() {
    let model = GatedModel::scripted(vec![text_response(&["read it"])]);
    with_ui(model.clone(), setup(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // 只输入 `@`：Puffin 的「Files」组，工作区顶上的前几个文件（文件夹不列）。
        ui.type_text("look at @");
        ui.wait("the first files", |s| {
            s.contains("Files") && s.contains("README.md") && !s.contains("src/")
        })
        .await;
        ui.type_text("mai");
        // 面板照 Puffin 的 `@`（tui-kit.md §4.2）：名字一列、它所在的文件夹在后面，槽里是 `⏎ insert`。
        ui.wait("the match", |s| {
            shows_file(s, "main.rs", "src") && s.contains("⏎ insert")
        })
        .await;
        // ⇥ 与 ⏎ 一样：整个词换成路径加空格（Puffin `at_key`；以前 ⇥ 先补公共前缀）。
        ui.key(KeyCode::Tab);
        ui.wait_for("look at @src/main.rs").await;
        ui.type_text("please");
        ui.wait_for("@src/main.rs please").await;
        ui.key(KeyCode::Enter);
        ui.wait("the reply", |s| {
            s.contains("read it") && !s.contains("esc stop")
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
        ui.wait("the match", |s| shows_file(s, "lib.rs", "src"))
            .await;
        ui.key(KeyCode::Esc);
        ui.wait("the list closed", |s| !shows_file(s, "lib.rs", "src"))
            .await;
        ui.type_text("zzz");
        let screen = ui.wait_for("@libzzz").await;
        assert!(!screen.contains("src/"), "{screen}");
    })
    .await;
}

/// 宽的终端里选中的文件在右边预览（Puffin「右栏」）：名字、路径、空一行、行数与前几行；读法由入口的 `peek_file` 给（这里是
/// 替身，读 MemoryEnvironment 的同一份内容）。文件夹的预览从清单数，不经入口。
#[tokio::test(flavor = "multi_thread")]
async fn the_chosen_file_is_previewed_on_the_right() {
    use rust_cli_tui_kit::app::widgets::at_panel::data::PathPeek;
    let mut setup = setup();
    setup.width = 120;
    setup.services.peek_file = Some(Arc::new(|_workspace, path: String| {
        Box::pin(async move {
            match path.as_str() {
                "src/main.rs" => PathPeek::File {
                    lines: 1,
                    more: false,
                    binary: false,
                    bytes: 13,
                    head: vec!["fn main() {}".to_owned()],
                },
                _ => PathPeek::Missing,
            }
        })
    }));
    let model = GatedModel::scripted(vec![]);
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("@main");
        let screen = ui
            .wait("the preview", |s| {
                s.contains("1 line") && s.contains(" 1  fn main() {}")
            })
            .await;
        // 右栏的第一行（与组名同一行）是名字，第二行是它所在的文件夹。
        let words = |l: &str| l.split_whitespace().map(str::to_owned).collect::<Vec<_>>();
        let at = screen
            .lines()
            .position(|l| words(l) == ["Files", "main.rs"]);
        let at = at.unwrap_or_else(|| panic!("{screen}"));
        let next = screen.lines().nth(at + 1).map(words);
        assert_eq!(
            next.as_deref(),
            Some(&["main.rs", "src", "src/"].map(str::to_owned)[..]),
            "{screen}"
        );
        for _ in 0.."main".len() {
            ui.key(KeyCode::Backspace);
        }
        ui.type_text("src/");
        ui.wait("the folder", |s| {
            s.contains("lib.rs") && s.contains("main.rs")
        })
        .await;
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
            s.contains("a picture") && !s.contains("esc stop")
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
