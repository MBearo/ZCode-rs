//! 修复线 F25（docs/specs/rust-cli/tui-select.md §4）：滚轮按最上层的可滚动视图分发，没有时滚对话区。
use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, Ui, with_ui};

/// 屏幕上第 `row` 行的文字。
fn line(screen: &str, row: usize) -> String {
    screen.lines().nth(row).unwrap_or_default().to_owned()
}

/// 滚轮向下滚几格后，`row` 行的文字变了（页面往下滚了），再滚回去又回到原样。
async fn scrolls(ui: &mut Ui, row: usize, what: &str) {
    let before = ui.screen();
    let top = line(&before, row);
    ui.wheel(true, 3, 10, 5);
    let after = ui
        .wait(&format!("{what} to scroll down"), |s| line(s, row) != top)
        .await;
    assert_ne!(line(&after, row), top, "{after}");
    ui.wheel(false, 20, 10, 5);
    ui.wait(&format!("{what} to scroll back"), |s| line(s, row) == top)
        .await;
}

fn short() -> Setup {
    Setup {
        height: 12,
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_context_page_scrolls() {
    let setup = Setup {
        height: 12,
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            // `/context all` 的页（page_frame）：正文在输入框与状态栏上面，看不见的行写成 `↑/↓ n more`。
            ui.submit("/context all");
            ui.wait_for("Stats  ›  Context").await;
            scrolls(&mut ui, 3, "the context page").await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_usage_page_scrolls() {
    let setup = Setup {
        height: 12,
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/usage all");
            ui.wait_for("This session").await;
            scrolls(&mut ui, 3, "the usage page").await;
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_transcript_scrolls_in_both_looks() {
    let reply: String = (0..40).map(|n| format!("reply line {n:02}\n")).collect();
    let model = GatedModel::scripted(vec![text_response(&[reply.as_str(), "done"])]);
    with_ui(model, short(), |mut ui| async move {
        ui.submit("hi");
        ui.wait("the reply", |s| {
            s.contains("done") && !s.contains("esc stop")
        })
        .await;
        // 对话区：向上滚之后最后一行不在屏幕上，滚回来又出现。
        ui.wheel(false, 6, 10, 3);
        ui.wait("the transcript to scroll up", |s| {
            !s.contains("reply line 39")
        })
        .await;
        ui.wheel(true, 20, 10, 3);
        ui.wait_for("reply line 39").await;
        // ctrl+o 在原地换成详细的样子（Puffin `details_open`），滚轮照样滚 transcript。以前是另一个全屏视图自己的滚动。
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        ui.wheel(false, 6, 10, 3);
        ui.wait("the detailed transcript to scroll up", |s| {
            !s.contains("reply line 39") && !s.contains("Showing detailed transcript")
        })
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_picker_moves_its_selection_and_leaves_the_transcript() {
    let reply: String = (0..40).map(|n| format!("reply line {n:02}\n")).collect();
    let model = GatedModel::scripted(vec![text_response(&[reply.as_str(), "done"])]);
    let setup = Setup {
        height: 24,
        ..Setup::default()
    };
    let chosen = setup.colors.ink().selection().fg_color();
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hi");
        ui.wait("the reply", |s| {
            s.contains("done") && !s.contains("esc stop")
        })
        .await;
        ui.submit("/model");
        // 打开时选中当前的模型（scripted）；照 Puffin 的选择器，选中只是名字的颜色（tui-kit.md §4.2）。
        // 画面的文字先到，格子的快照随后：等快照里也有它。
        let before = loop {
            let shot = ui.frame();
            if let Some((x, y)) = shot.find("other-model") {
                break shot.style_at(y, x).fg_color();
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        };
        assert_ne!(before, chosen, "{}", ui.screen());
        ui.wheel(true, 1, 10, 20);
        wait_chosen(&ui, "other-model", chosen).await;
        let after = ui.screen();
        // 列表拿走了滚轮：对话区停在底部，没有分隔行（层盖在正文最新的几行上，不推它们，tui-kit.md §4.0）。
        assert!(after.contains("reply line 31"), "{after}");
        assert!(!after.contains("back to the end"), "{after}");
    })
    .await;
}

/// 滚上去之后分隔行写 `^N`，真按 ctrl+n 回到末尾（tui-kit.md §4.0）；草稿不动。
#[tokio::test(flavor = "multi_thread")]
async fn ctrl_n_follows_the_separator_hint_back_to_the_end() {
    let reply: String = (0..200).map(|n| format!("reply line {n:03}\n")).collect();
    let model = GatedModel::scripted(vec![text_response(&[reply.as_str(), "done"])]);
    let setup = Setup {
        height: 24,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hi");
        ui.wait("the reply", |s| {
            s.contains("done") && !s.contains("esc stop")
        })
        .await;
        ui.type_text("draft");
        ui.wait_for("draft").await;
        ui.wheel(false, 3, 10, 5);
        let held = ui.wait_for("^N back to the end").await;
        assert!(!held.contains("^End"), "{held}");
        ui.press(KeyCode::Char('n'), KeyModifiers::CONTROL);
        let back = ui
            .wait("the view to follow", |s| !s.contains("back to the end"))
            .await;
        assert!(back.contains("done"), "{back}");
        assert!(back.contains("draft"), "{back}");
    })
    .await;
}

/// 选中的那一项（S3 起照 Puffin 的设置页：选中只是名字的颜色，tui-kit.md §4.2「`/permissions`」）：等 `needle` 的第一个
/// 字变成选中色。等待有上限（同 `Ui::wait`）。
async fn wait_chosen(ui: &Ui, needle: &str, chosen: rust_cli_tui_kit::render::Color) {
    let wait = async {
        loop {
            let shot = ui.frame();
            if shot
                .find(needle)
                .is_some_and(|(x, y)| shot.style_at(y, x).fg_color() == chosen)
            {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!("{needle} is not chosen:\n{}", ui.screen());
    }
}
