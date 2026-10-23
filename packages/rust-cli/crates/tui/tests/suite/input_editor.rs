//! 外部编辑器与 `/memory`（docs/specs/rust-cli/tui-input.md §4）：ctrl+g、ctrl+x ctrl+e 交出终端、编辑器改写输入、
//! 收回后整屏重画；`/memory` 列出规则文件，选中后用编辑器打开并给出提示。

use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_tui::{Editor, MemoryFile, Services};

use super::support::{GatedModel, Setup, with_ui};

/// 编辑器替身：`edit` 在文字后面加 ` edited` 和换行；`fail` 时以 `vim exited with code 1` 失败；`crlf` 时像 Windows
/// 的编辑器那样写 CRLF（加一行 `second line`）。
#[derive(Default)]
struct FakeEditor {
    edits: Mutex<Vec<String>>,
    opened: Mutex<Vec<String>>,
    fail: bool,
    crlf: bool,
}

impl Editor for FakeEditor {
    fn edit(&self, text: String) -> BoxFuture<'static, Result<String, String>> {
        self.edits.lock().unwrap().push(text.clone());
        let (fail, crlf) = (self.fail, self.crlf);
        async move {
            if fail {
                Err("vim exited with code 1".to_owned())
            } else if crlf {
                Ok(format!("{text}\r\nsecond line\r\n"))
            } else {
                Ok(format!("{text} edited\n"))
            }
        }
        .boxed()
    }

    fn open(&self, path: String) -> BoxFuture<'static, Result<(), String>> {
        self.opened.lock().unwrap().push(path);
        async { Ok(()) }.boxed()
    }

    fn source(&self) -> Option<(String, String)> {
        Some(("$EDITOR".to_owned(), "vim".to_owned()))
    }
}

fn setup(editor: Arc<FakeEditor>) -> Setup {
    let file = |label: &str, display: &str, path: &str| MemoryFile {
        label: label.into(),
        display: display.into(),
        path: path.into(),
    };
    Setup {
        services: Services {
            editor: Some(editor),
            memory_files: vec![
                file(
                    "User memory",
                    "~/.zcode/AGENTS.md",
                    "/Users/dev/.zcode/AGENTS.md",
                ),
                file(
                    "Project memory",
                    "./AGENTS.md",
                    "/Users/dev/project/AGENTS.md",
                ),
            ],
            ..Services::default()
        },
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ctrl_g_hands_the_terminal_over_and_puts_the_text_back() {
    let editor = Arc::new(FakeEditor::default());
    let model = GatedModel::scripted(vec![]);
    let handovers = with_handovers(model, setup(editor.clone()), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("draft");
        ui.wait_for("draft").await;
        ui.press(KeyCode::Char('g'), KeyModifiers::CONTROL);
        ui.wait_for("draft edited").await;
        // 和弦 ctrl+x ctrl+e 同样打开编辑器。
        ui.press(KeyCode::Char('x'), KeyModifiers::CONTROL);
        ui.press(KeyCode::Char('e'), KeyModifiers::CONTROL);
        ui.wait_for("draft edited edited").await;
    })
    .await;
    assert_eq!(
        editor.edits.lock().unwrap().as_slice(),
        ["draft", "draft edited"]
    );
    // 交出期间不画帧：suspend 与 resume 之间没有 draw，收回后立即重画。
    let first = handovers.iter().position(|e| *e == "suspend").unwrap();
    assert_eq!(handovers[first + 1], "resume");
    assert_eq!(handovers[first + 2], "draw");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failing_editor_keeps_the_input_and_explains() {
    let editor = Arc::new(FakeEditor {
        fail: true,
        ..FakeEditor::default()
    });
    let model = GatedModel::scripted(vec![]);
    with_ui(model, setup(editor), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("keep me");
        ui.press(KeyCode::Char('g'), KeyModifiers::CONTROL);
        let screen = ui.wait_for("vim exited with code 1").await;
        assert!(screen.contains("keep me"), "{screen}");
    })
    .await;
}

/// 修复线 F65 的 T4：编辑器写回 CRLF（Windows 的记事本、VS Code）。读回时统一换行：输入框里是两行，模型收到的文字
/// 没有 `\r`。
#[tokio::test(flavor = "multi_thread")]
async fn crlf_from_the_editor_reaches_the_model_as_plain_newlines() {
    let editor = Arc::new(FakeEditor {
        crlf: true,
        ..FakeEditor::default()
    });
    let model = GatedModel::scripted(vec![rust_cli_kernel_api::testing::text_response(&[
        "noted",
    ])]);
    with_ui(model.clone(), setup(editor), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.type_text("first line");
        ui.wait_for("first line").await;
        ui.press(KeyCode::Char('g'), KeyModifiers::CONTROL);
        ui.wait_for("second line").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("noted").await;
    })
    .await;
    let sent = super::support::request_text(&model.requests()[0]);
    assert!(sent.contains("first line\\nsecond line"), "{sent}");
    assert!(!sent.contains("\\r"), "{sent}");
}

#[tokio::test(flavor = "multi_thread")]
async fn memory_opens_the_chosen_file_and_esc_cancels() {
    let editor = Arc::new(FakeEditor::default());
    let model = GatedModel::scripted(vec![]);
    with_ui(model, setup(editor.clone()), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/memory");
        ui.wait_for("User memory").await;
        ui.key(KeyCode::Esc);
        ui.wait_for("Cancelled memory editing").await;
        ui.submit("/memory");
        ui.wait_for("Project memory").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Opened memory file at ./AGENTS.md").await;
        assert!(
            screen.contains("> Using $EDITOR=\"vim\". To change editor, set $EDITOR or $VISUAL"),
            "{screen}"
        );
    })
    .await;
    assert_eq!(
        editor.opened.lock().unwrap().as_slice(),
        ["/Users/dev/project/AGENTS.md"]
    );
}

/// 跑界面并返回画帧与交出终端的顺序。
async fn with_handovers<F, Fut>(
    model: Arc<GatedModel>,
    setup: Setup,
    script: F,
) -> Vec<&'static str>
where
    F: FnOnce(super::support::Ui) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    let log = Arc::new(Mutex::new(Vec::new()));
    let seen = log.clone();
    with_ui(model, setup, |ui| async move {
        let handovers = ui.handovers.clone();
        script(ui).await;
        *seen.lock().unwrap() = handovers.lock().unwrap().clone();
    })
    .await;
    log.lock().unwrap().clone()
}
