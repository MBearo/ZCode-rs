//! 图片粘贴（docs/specs/rust-cli/tui-input.md §3）：ctrl+v（Windows alt+v）经剪贴板服务读图片，光标处插入
//! `[Image #N]`；拖进来的图片路径同样处理；发送后模型收到文字块与 image 块；剪贴板没有图片时给出提示。

use std::sync::Arc;

use base64::Engine;
use crossterm::event::{KeyCode, KeyModifiers};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::{Clipboard, ImageBytes, Services};

use super::support::{GatedModel, Setup, request_text, with_ui};

const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000IHDR";

/// 剪贴板替身：`image` 给出 `clipboard`；`image_file` 只认 `/Users/dev/shot.png`。
struct FakeClipboard {
    clipboard: Option<Vec<u8>>,
}

impl Clipboard for FakeClipboard {
    fn image(&self) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        let bytes = self.clipboard.clone();
        async move { Ok(bytes.map(|bytes| ImageBytes { bytes })) }.boxed()
    }

    fn image_file(&self, path: String) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        async move {
            Ok((path == "/Users/dev/my shot.png").then(|| ImageBytes {
                bytes: PNG.to_vec(),
            }))
        }
        .boxed()
    }
}

fn setup(clipboard: Option<Vec<u8>>) -> Setup {
    Setup {
        services: Services {
            clipboard: Some(Arc::new(FakeClipboard { clipboard })),
            ..Services::default()
        },
        ..Setup::default()
    }
}

/// 粘贴图片的键（Windows alt+v，其他平台 ctrl+v）。
fn paste_key(ui: &super::support::Ui) {
    if cfg!(windows) {
        ui.press(KeyCode::Char('v'), KeyModifiers::ALT);
    } else {
        ui.press(KeyCode::Char('v'), KeyModifiers::CONTROL);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pasted_image_reaches_the_model_after_the_text() {
    let model = GatedModel::scripted(vec![text_response(&["a tiny png"])]);
    with_ui(
        model.clone(),
        setup(Some(PNG.to_vec())),
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.type_text("what is ");
            paste_key(&ui);
            ui.wait_for("what is [Image #1]").await;
            ui.submit("please");
            ui.wait("the reply", |s| {
                s.contains("a tiny png") && !s.contains("esc to interrupt")
            })
            .await;
        },
    )
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 1);
    let text = request_text(&requests[0]);
    let data = base64::engine::general_purpose::STANDARD.encode(PNG);
    assert!(text.contains("what is [Image #1] please"), "{text}");
    assert!(text.contains(&data), "{text}");
    // 文字块在前，图片在后。
    assert!(text.find("[Image #1] please") < text.find(&data), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn an_empty_clipboard_explains_and_paths_become_images() {
    let model = GatedModel::scripted(vec![]);
    with_ui(model, setup(None), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        paste_key(&ui);
        let key = if cfg!(windows) { "alt+v" } else { "ctrl+v" };
        ui.wait_for(&format!(
            "No image found in clipboard. Use {key} to paste images."
        ))
        .await;
        // 拖进来的图片路径（带 `\ ` 转义）：读出文件后当图片。
        ui.paste("/Users/dev/my\\ shot.png");
        ui.wait_for("[Image #1]").await;
        // 读不出的路径照常当文字粘贴；紧跟图片粘贴的文字不补空格（只在链接之后补，修复线 F13）。
        ui.paste("/Users/dev/other.png");
        ui.wait_for("[Image #1]/Users/dev/other.png").await;
    })
    .await;
}

/// 修复线 F26（tui-input.md §3.1）：`[Image #N]` 是一个整体：← 整块跳过，在它末尾按 backspace
/// 一次删掉整块，图片也一起去掉（模型收不到 image 块）；撤销让占位文字与图片一起回来。旧代码一次删一个字符，
/// 删完还剩 `[Image #1`，图片仍随请求发出。
#[tokio::test(flavor = "multi_thread")]
async fn backspace_removes_the_whole_placeholder_and_its_image() {
    let model = GatedModel::scripted(vec![
        text_response(&["no picture"]),
        text_response(&["with picture"]),
    ]);
    with_ui(
        model.clone(),
        setup(Some(PNG.to_vec())),
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.type_text("look at ");
            paste_key(&ui);
            ui.wait_for("look at [Image #1]").await;
            // ← 一次越过整个占位文字，delete 在开头一次删掉整块；撤销找回。
            ui.key(KeyCode::Left);
            ui.key(KeyCode::Delete);
            ui.wait("the placeholder to go", |s| {
                s.contains("look at") && !s.contains("[Image")
            })
            .await;
            ui.press(KeyCode::Char('_'), KeyModifiers::CONTROL);
            ui.wait_for("look at [Image #1]").await;
            // backspace 在末尾一次删掉整块。
            ui.key(KeyCode::End);
            ui.key(KeyCode::Backspace);
            let screen = ui
                .wait("the placeholder to go", |s| {
                    s.contains("look at") && !s.contains("[Image")
                })
                .await;
            assert!(!screen.contains("#1"), "{screen}");
            ui.submit("it");
            ui.wait("the first reply", |s| {
                s.contains("no picture") && !s.contains("esc to interrupt")
            })
            .await;
            // 撤销找回：再粘一张，删掉，ctrl+_ 撤销，图片一起回来。
            paste_key(&ui);
            ui.wait_for("[Image #2]").await;
            ui.key(KeyCode::Backspace);
            ui.wait("the placeholder to go", |s| !s.contains("[Image #2]"))
                .await;
            ui.press(KeyCode::Char('_'), KeyModifiers::CONTROL);
            ui.wait_for("[Image #2]").await;
            ui.key(KeyCode::Enter);
            ui.wait("the second reply", |s| {
                s.contains("with picture") && !s.contains("esc to interrupt")
            })
            .await;
        },
    )
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    let data = base64::engine::general_purpose::STANDARD.encode(PNG);
    let first = request_text(&requests[0]);
    // 粘贴图片后的第一个键入字符前补空格（这个标记只由键入或粘贴清掉，删掉图片不清，tui-input.md §3.1）。
    assert!(first.contains("look at  it"), "{first}");
    assert!(
        !first.contains(&data),
        "the deleted image is not sent: {first}"
    );
    let second = request_text(&requests[1]);
    assert!(
        second.contains(&data),
        "undo brought the image back: {second}"
    );
}
