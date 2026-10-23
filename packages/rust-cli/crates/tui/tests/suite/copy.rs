//! 修复线 F25（docs/specs/rust-cli/tui-select.md）：模型请求失败的错误行完整显示；鼠标拖选在对话区与全屏视图里
//! 选择并在松开时复制（OSC 52 与入口的剪贴板服务），提示文字固定。
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use ratatui::style::Color;
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::ui::palette::{ColorLevel, Palette};
use rust_cli_tui::{Clipboard, CopyRoute, ImageBytes};

use super::support::{GatedModel, Setup, row_of, squash, with_ui};

/// 用户报告里那条 Bedrock 的 400（请求 ID 换成示例值）。
const BEDROCK: &str = "InvokeModelWithResponseStream: operation error Bedrock Runtime: InvokeModelWithResponseStream, https response error StatusCode: 400, RequestID: 0f0e0d0c-1111-2222-3333-444455556666, ValidationException: system.1.cache_control.ephemeral.scope: Extra inputs are not permitted (request id: req_0123456789abcdef)";

fn failing(message: &str) -> Arc<GatedModel> {
    GatedModel::scripted(vec![vec![Err(ModelError::InvalidRequest {
        message: message.into(),
    })]])
}

fn wide() -> Setup {
    Setup {
        width: 100,
        height: 30,
        ..Setup::default()
    }
}

/// 剪贴板替身：记下写进来的文字，按给定的途径回答。
#[derive(Default)]
struct FakeClipboard {
    route: Option<CopyRoute>,
    texts: Mutex<Vec<String>>,
}

impl Clipboard for FakeClipboard {
    fn image(&self) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        async { Ok(None) }.boxed()
    }

    fn image_file(&self, _path: String) -> BoxFuture<'static, Result<Option<ImageBytes>, String>> {
        async { Ok(None) }.boxed()
    }

    fn copy_text(&self, text: String) -> BoxFuture<'static, CopyRoute> {
        self.texts.lock().unwrap().push(text);
        let route = self.route.unwrap_or(CopyRoute::Native);
        async move { route }.boxed()
    }
}

/// 第 `row` 行里 `needle` 所在的显示列。
fn column_of(screen: &str, row: u16, needle: &str) -> u16 {
    let line = screen.lines().nth(usize::from(row)).unwrap();
    let byte = line.find(needle).unwrap();
    u16::try_from(unicode_width::UnicodeWidthStr::width(&line[..byte])).unwrap()
}

fn with_clipboard(clipboard: &Arc<FakeClipboard>, mut setup: Setup) -> Setup {
    setup.services.clipboard = Some(clipboard.clone());
    setup
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_model_error_is_shown_in_full() {
    with_ui(failing(BEDROCK), wide(), |mut ui| async move {
        ui.submit("hi");
        let screen = ui
            .wait("the error line", |s| {
                s.contains("invalid request") && !s.contains("esc to interrupt")
            })
            .await;
        // 缺陷（F25）：原来错误行截到一行宽，后半句与请求 ID 看不到。
        assert!(
            squash(&screen).contains(&squash(&format!("invalid request: {BEDROCK}"))),
            "{screen}"
        );
        assert!(!screen.contains("https…"), "{screen}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_error_over_a_thousand_characters_is_cut_until_ctrl_o() {
    let long = format!("{} END-OF-ERROR", "x".repeat(1100));
    with_ui(failing(&long), wide(), |mut ui| async move {
        ui.submit("hi");
        let screen = ui.wait_for("(ctrl+o to expand)").await;
        assert!(!screen.contains("END-OF-ERROR"), "{screen}");
        assert!(screen.contains('…'), "{screen}");
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        ui.wait_for("END-OF-ERROR").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dragging_over_the_error_copies_the_whole_message() {
    let clipboard = Arc::new(FakeClipboard::default());
    let check = clipboard.clone();
    with_ui(
        failing(BEDROCK),
        with_clipboard(&clipboard, wide()),
        |mut ui| async move {
            ui.submit("hi");
            let screen = ui
                .wait("the error line", |s| {
                    s.contains("invalid request") && !s.contains("esc to interrupt")
                })
                .await;
            let first = row_of(&screen, "⏺ invalid request").expect("error row");
            let lines: Vec<&str> = screen.lines().collect();
            let mut last = first;
            while lines
                .get(usize::from(last) + 1)
                .is_some_and(|line| line.starts_with("  ") && !line.trim().is_empty())
            {
                last += 1;
            }
            assert!(last > first, "the error wraps: {screen}");
            ui.drag((2, first), (99, last));
            ui.wait_for("copied").await;
            let expected = format!("invalid request: {BEDROCK}");
            // 软折行接起来：续行的缩进不取，折行处的空格补回。
            assert_eq!(ui.copied(), vec![expected.clone()]);
            assert_eq!(*check.texts.lock().unwrap(), vec![expected.clone()]);
            let screen = ui.screen();
            assert!(
                screen.contains(&format!(
                    "copied {} chars to clipboard",
                    expected.chars().count()
                )),
                "{screen}"
            );
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_status_page_can_be_selected_and_copied() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/status");
        let page = ui.wait_for("Directory:").await;
        let row = row_of(&page, "Directory:").unwrap();
        ui.drag((2, row), (79, row));
        // 缺陷（F25）：原来全屏视图里按下鼠标不建选区，什么都复制不到。
        ui.wait_for("sent").await;
        let copied = ui.copied();
        assert_eq!(copied.len(), 1, "{copied:?}");
        assert!(copied[0].starts_with("Directory:"), "{copied:?}");
        assert!(copied[0].ends_with("~/project"), "{copied:?}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn releasing_copies_without_a_footer_hint_and_ctrl_c_only_clears() {
    let model = GatedModel::scripted(vec![text_response(&["hello world"])]);
    let setup = Setup {
        width: 120,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hi");
        let screen = ui
            .wait("the reply", |s| {
                s.contains("hello world") && !s.contains("esc to interrupt")
            })
            .await;
        let row = row_of(&screen, "hello world").unwrap();
        let col = column_of(&screen, row, "hello");
        ui.drag((col, row), (col + 4, row));
        let screen = ui.wait_for("sent 5 chars via OSC 52").await;
        // 没有注入剪贴板服务：途径是 OSC 52，提示带按住修饰键的说明。
        assert!(screen.contains("if paste fails, hold"), "{screen}");
        // 松开即复制（用户决定）：footer 不显示选区提示。
        assert!(!screen.contains("copy & follow"), "{screen}");
        assert!(!screen.contains("ctrl+c copy"), "{screen}");
        assert_eq!(ui.copied(), vec!["hello".to_owned()]);
        // ctrl+c：这个选区已经复制过，只清掉选区，不再写剪贴板，也不进入“再按一次退出”。
        ui.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        ui.press(KeyCode::Char('x'), KeyModifiers::NONE);
        let screen = ui.wait_for("❯ x").await;
        assert!(!screen.contains("again to exit"), "{screen}");
        assert_eq!(ui.copied(), vec!["hello".to_owned()]);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_selection_uses_the_theme_background_and_chinese_notices() {
    let model = GatedModel::scripted(vec![text_response(&["hello world"])]);
    let setup = Setup {
        width: 120,
        lang: "zh_CN.UTF-8",
        palette: Palette::new(ColorLevel::TrueColor, Some((255, 255, 255))),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hi");
        let screen = ui
            .wait("the reply", |s| {
                s.contains("hello world") && !s.contains("esc to interrupt")
            })
            .await;
        let row = row_of(&screen, "hello world").unwrap();
        let col = column_of(&screen, row, "hello");
        ui.drag((col, row), (col + 4, row));
        ui.wait_for("已通过 OSC 52 发送 5 个字符").await;
        let buffer = ui.buffer.lock().unwrap().clone().unwrap();
        // 浅色主题的选区底色，前景不变。
        assert_eq!(buffer[(col, row)].bg, Color::Rgb(180, 213, 255));
        assert_ne!(buffer[(col + 5, row)].bg, Color::Rgb(180, 213, 255));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_n_goes_through_the_clipboard_service() {
    let clipboard = Arc::new(FakeClipboard {
        route: Some(CopyRoute::TmuxBuffer),
        ..FakeClipboard::default()
    });
    let check = clipboard.clone();
    let model = GatedModel::scripted(vec![text_response(&["the reply"])]);
    with_ui(
        model,
        with_clipboard(&clipboard, Setup::default()),
        |mut ui| async move {
            ui.submit("hi");
            ui.wait("the reply", |s| {
                s.contains("the reply") && !s.contains("esc to interrupt")
            })
            .await;
            ui.submit("/copy");
            ui.wait_for("Copied the last reply").await;
            assert_eq!(ui.copied(), vec!["the reply".to_owned()]);
            assert_eq!(*check.texts.lock().unwrap(), vec!["the reply".to_owned()]);
        },
    )
    .await;
}
