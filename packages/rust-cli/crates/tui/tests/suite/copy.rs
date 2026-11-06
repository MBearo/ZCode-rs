//! 修复线 F25（docs/specs/rust-cli/tui-select.md）：模型请求失败的错误行完整显示；鼠标拖选在对话区与全屏视图里
//! 选择并在松开时复制（OSC 52 与入口的剪贴板服务），提示文字固定。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::model::ModelError;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::{ColorLevel, Colors, CopyRoute};

use super::stub_services::FakeClipboard;
use super::support::{GatedModel, Setup, Ui, row_of, squash, with_ui};

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

/// 第 `row` 行里 `needle` 所在的显示列。
fn column_of(screen: &str, row: u16, needle: &str) -> u16 {
    let line = screen.lines().nth(usize::from(row)).unwrap();
    let byte = line.find(needle).unwrap();
    u16::try_from(rust_cli_tui_kit::render::width::string_width(&line[..byte])).unwrap()
}

fn with_clipboard(clipboard: &Arc<FakeClipboard>, mut setup: Setup) -> Setup {
    setup.services.clipboard = clipboard.clone();
    setup
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_model_error_is_shown_in_full() {
    with_ui(failing(BEDROCK), wide(), |mut ui| async move {
        ui.submit("hi");
        let screen = ui
            .wait("the error line", |s| {
                s.contains("invalid request") && !s.contains("esc stop")
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

/// 很长的错误也整段写在失败块里（Puffin `failures::block`）。以前过 1000 字截断、ctrl+o 展开，Puffin 没有这一步。
#[tokio::test(flavor = "multi_thread")]
async fn an_error_over_a_thousand_characters_is_shown_in_full() {
    let long = format!("{} END-OF-ERROR", "x".repeat(1100));
    with_ui(failing(&long), wide(), |mut ui| async move {
        ui.submit("hi");
        let screen = ui.wait_for("END-OF-ERROR").await;
        assert!(screen.contains(" ✗ The model request failed."), "{screen}");
        assert!(!screen.contains("^O to expand"), "{screen}");
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
                    s.contains("invalid request") && !s.contains("esc stop")
                })
                .await;
            // 失败块：头一行是句子，下面从第 3 列起是原话（Puffin `failures::block`）。
            let first = row_of(&screen, "   invalid request").expect("error row");
            let lines: Vec<&str> = screen.lines().collect();
            let mut last = first;
            while lines
                .get(usize::from(last) + 1)
                .is_some_and(|line| line.starts_with("  ") && !line.trim().is_empty())
            {
                last += 1;
            }
            assert!(last > first, "the error wraps: {screen}");
            // 最后一行在原话之后是失败块的事实（`  ·  HTTP 400`）：拖到原话的最后一个字为止。
            let end = lines[usize::from(last)]
                .find("  ·  HTTP")
                .map_or(99, |at| lines[usize::from(last)][..at].chars().count() - 1);
            ui.drag((2, first), (u16::try_from(end).unwrap(), last));
            let expected = format!("invalid request: {BEDROCK}");
            // 系统剪贴板：槽里是 Puffin 的 `Copied N characters`（tui-kit.md §4.0「板」的槽）。
            ui.wait_for(&format!("Copied {} characters", expected.chars().count()))
                .await;
            // 软折行接起来：续行的缩进不取，折行处的空格补回。
            assert_eq!(ui.copied(), vec![expected.clone()]);
            assert_eq!(*check.texts.lock().unwrap(), vec![expected.clone()]);
        },
    )
    .await;
}

// 页与板上的卡不能拖选（Puffin 的规则：选区只记 transcript 的行，docs/puffin-tui.md「选择与复制」）；以前全屏的页可以
// （F25），现在由页自己的键复制整页（`c`，设置页 `^Y`，tui-kit.md §4.3）。

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
                s.contains("hello world") && !s.contains("esc stop")
            })
            .await;
        let row = row_of(&screen, "hello world").unwrap();
        let col = column_of(&screen, row, "hello");
        ui.drag((col, row), (col + 4, row));
        let screen = ui.wait_for("Copied 5 characters").await;
        // 没有注入剪贴板服务：途径是 OSC 52。槽照 Puffin 只说 `Copied N characters`；途径的说明（HEAD 的能力，
        // tui-select.md §3.3）在输入框上方安静的一行，告诉你粘贴不出来时怎么办。
        assert!(
            screen.contains("Sent via OSC 52 · if paste fails, hold"),
            "{screen}"
        );
        // 松开即复制（用户决定）：footer 不显示选区提示。
        assert!(!screen.contains("copy & follow"), "{screen}");
        assert!(!screen.contains("ctrl+c copy"), "{screen}");
        assert_eq!(ui.copied(), vec!["hello".to_owned()]);
        // ctrl+c：这个选区已经复制过，只清掉选区，不再写剪贴板，也不进入“再按一次退出”。
        ui.press(KeyCode::Char('c'), KeyModifiers::CONTROL);
        ui.press(KeyCode::Char('x'), KeyModifiers::NONE);
        let screen = ui.wait_for("› x").await;
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
        colors: Colors::new(ColorLevel::TrueColor, Some((255, 255, 255)))
            .with_foreground(Some((0, 0, 0))),
        ..Setup::default()
    };
    // 浅色的 rich 主题的选区底色：Puffin 的 `highlight`，`line` 的前景色（`Ink::selection_bg`，tui-kit.md §3.4）。
    let selection = rust_cli_tui::Ink::new(
        ColorLevel::TrueColor,
        Some((0, 0, 0)),
        Some((255, 255, 255)),
    )
    .selection_bg()
    .expect("rich has a selection ground");
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hi");
        let screen = ui
            .wait("the reply", |s| {
                s.contains("hello world") && !s.contains("esc stop")
            })
            .await;
        let row = row_of(&screen, "hello world").unwrap();
        let col = column_of(&screen, row, "hello");
        ui.drag((col, row), (col + 4, row));
        ui.wait_for("已复制 5 个字符").await;
        let buffer = ui.frame();
        // 浅色主题的选区底色，前景不变。
        assert_eq!(buffer[(col, row)].bg, selection);
        assert_ne!(buffer[(col + 5, row)].bg, selection);
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
                s.contains("the reply") && !s.contains("esc stop")
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

/// 照 Puffin 的连击（docs/puffin-tui.md §4.4「连击」「词」「行」，kit `select_units`）：同一处 500 ms 内的第二下选词——路径与
/// `文件:行号` 是一个词，句末的点不算；第三下选整个逻辑行；双击后拖动按整词扩大。以前双击按旧的字符类选、三击只选屏幕上的
/// 一行，双击、三击之后不能拖动。
#[tokio::test(flavor = "multi_thread")]
async fn clicks_pick_words_and_lines_like_puffin() {
    use crossterm::event::{MouseButton, MouseEventKind};
    let line = "Open src/app.rs:12. Then run the tests";
    let model = GatedModel::scripted(vec![text_response(&[line])]);
    with_ui(model, wide(), |mut ui| async move {
        ui.submit("hi");
        let screen = ui
            .wait("the reply", |s| {
                s.contains("run the tests") && !s.contains("esc stop")
            })
            .await;
        let row = row_of(&screen, "src/app.rs").unwrap();
        let at = |needle| column_of(&screen, row, needle);
        let press = |ui: &Ui, col| ui.mouse(MouseEventKind::Down(MouseButton::Left), col, row);
        let lift = |ui: &Ui, col| ui.mouse(MouseEventKind::Up(MouseButton::Left), col, row);
        let (app, then, open, run) = (at("app"), at("Then"), at("Open"), at("run") + 1);
        press(&ui, app);
        lift(&ui, app);
        press(&ui, app);
        lift(&ui, app);
        ui.wait_for("Copied 13 characters").await;
        assert_eq!(
            ui.copied().last().map(String::as_str),
            Some("src/app.rs:12")
        );
        for _ in 0..3 {
            press(&ui, then);
            lift(&ui, then);
        }
        ui.wait_for("Copied 38 characters").await;
        assert_eq!(ui.copied().last().map(String::as_str), Some(line));
        press(&ui, open);
        lift(&ui, open);
        press(&ui, open);
        ui.mouse(MouseEventKind::Drag(MouseButton::Left), run, row);
        lift(&ui, run);
        ui.wait_for("Copied 28 characters").await;
        assert_eq!(
            ui.copied().last().map(String::as_str),
            Some("Open src/app.rs:12. Then run")
        );
    })
    .await;
}

/// 页的复制（tui-kit.md §4.3「页的复制」）：页里不拖选，页的字由页自己的键复制，走 `/copy` 的同一个入口；槽里说
/// `Copied the page`，到下一个键。`c` 是代替正文的页的键，设置页打的字是它的搜索，按 ⌃y。
#[tokio::test(flavor = "multi_thread")]
async fn a_page_copies_its_text_with_its_own_key() {
    let clipboard = Arc::new(FakeClipboard::default());
    let setup = with_clipboard(&clipboard, wide());
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/stats all");
            // 等计数到了（页上是方格图与一句话，tui-panels.md §4.2）再复制。
            ui.wait_for("comes with every request").await;
            ui.key(KeyCode::Char('c'));
            ui.wait_for("Copied the page").await;
            let page = ui.copied().last().cloned().unwrap_or_default();
            // 整页的字：面包屑在第一行（首尾的空行与共同的缩进去掉），段在下面。
            assert!(page.starts_with("Stats  ›  Context"), "{page}");
            assert!(page.contains("comes with every request"), "{page}");
            assert!(!page.ends_with('\n'), "{page}");
            // 下一个键收起这一句；页还开着，`c` 不进草稿。
            ui.key(KeyCode::Down);
            let screen = ui
                .wait("the note to go", |s| !s.contains("Copied the page"))
                .await;
            assert!(screen.contains("Stats  ›  Context"), "{screen}");
            // esc 回到数字的卡（打字再关上它）。
            ui.key(KeyCode::Esc);
            ui.wait("the page to close", |s| !s.contains("Stats  ›"))
                .await;
            // 设置页：`c` 是搜索的字，⌃y 复制这一屏的页（不含输入框与状态栏）。
            ui.submit("/settings");
            ui.wait_for("Settings  ›").await;
            ui.press(KeyCode::Char('y'), KeyModifiers::CONTROL);
            ui.wait_for("Copied the page").await;
            let settings = ui.copied().last().cloned().unwrap_or_default();
            assert!(settings.contains("Settings  ›"), "{settings}");
            assert!(!settings.contains("Ask before changes"), "{settings}");
            // 两次都经剪贴板服务（同 `/copy`）。
            let sent = clipboard.texts.lock().unwrap().clone();
            assert_eq!(sent, vec![page, settings]);
        },
    )
    .await;
}
