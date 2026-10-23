//! 新会话的背景标志（docs/specs/rust-cli/tui.md §7.3，修复线 F26，复制自 Host TUI 2f982570bb）：新会话的空白处
//! 画 Z 镂空的金属徽章；点击后约 2 秒按动画节拍重画，结束后不再定时重画；有字、有会话时不画；深浅两种背景。
use std::time::Duration;

use ratatui::buffer::Buffer;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::ui::palette::{ColorLevel, Palette};

use super::support::{GatedModel, Setup, with_ui};

/// 盲文点阵字符（标志的点）所在的格子：（列，行）。
fn braille(buffer: &Buffer) -> Vec<(u16, u16)> {
    let area = buffer.area;
    let mut cells = Vec::new();
    for y in area.top()..area.bottom() {
        for x in area.left()..area.right() {
            let symbol = buffer[(x, y)].symbol();
            if symbol
                .chars()
                .next()
                .is_some_and(|c| ('\u{2801}'..='\u{28ff}').contains(&c))
            {
                cells.push((x, y));
            }
        }
    }
    cells
}

fn draws(handovers: &std::sync::Mutex<Vec<&'static str>>) -> usize {
    handovers
        .lock()
        .unwrap()
        .iter()
        .filter(|h| **h == "draw")
        .count()
}

fn setup(background: (u8, u8, u8)) -> Setup {
    Setup {
        width: 100,
        height: 40,
        palette: Palette::new(ColorLevel::TrueColor, Some(background)),
        ..Setup::default()
    }
}

/// 新会话时画出徽章；不点击时不定时重画；点击后约 2 秒里重画多帧，结束后停住、不再重画。
#[tokio::test(flavor = "multi_thread")]
async fn the_badge_animates_only_after_a_click() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, setup((0x1e, 0x1e, 0x2e)), |mut ui| async move {
        let buffer = ui.buffer.clone();
        ui.wait("the badge", |_| ui_braille(&buffer).len() > 300)
            .await;
        let cells = ui_braille(&ui.buffer);
        // 平时不动：没有定时重画。
        let before = draws(&ui.handovers);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(draws(&ui.handovers), before, "no redraw while idle");
        // 按在徽章右边：按动画节拍重画（约 30 fps），大约 2 秒后停住。
        let (x, y) = cells[cells.len() / 2];
        ui.mouse(
            crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
            x + 10,
            y,
        );
        tokio::time::sleep(Duration::from_millis(600)).await;
        let during = draws(&ui.handovers) - before;
        assert!(during >= 5, "redrawn during the press: {during}");
        tokio::time::sleep(Duration::from_millis(1800)).await;
        let settled = draws(&ui.handovers);
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(draws(&ui.handovers), settled, "no redraw after the press");
        assert!(ui_braille(&ui.buffer).len() > 300, "the badge stays");
    })
    .await;
}

fn ui_braille(buffer: &std::sync::Mutex<Option<Buffer>>) -> Vec<(u16, u16)> {
    buffer
        .lock()
        .unwrap()
        .as_ref()
        .map(braille)
        .unwrap_or_default()
}

/// 有字或有会话时不画；浅色背景用另一套金属色（比纸暗）。
#[tokio::test(flavor = "multi_thread")]
async fn the_badge_hides_behind_input_and_sessions_and_follows_the_theme() {
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    with_ui(model, setup((0xfa, 0xfa, 0xf7)), |mut ui| async move {
        let buffer = ui.buffer.clone();
        ui.wait("the badge", |_| ui_braille(&buffer).len() > 300)
            .await;
        // 浅色背景：点的颜色都比背景暗。
        {
            let guard = ui.buffer.lock().unwrap();
            let buffer = guard.as_ref().unwrap();
            for (x, y) in braille(buffer) {
                if let ratatui::style::Color::Rgb(r, g, b) = buffer[(x, y)].fg {
                    assert!(
                        u32::from(r) + u32::from(g) + u32::from(b) < 0xfa + 0xfa + 0xf7,
                        "({x}, {y})"
                    );
                }
            }
        }
        ui.type_text("hi");
        ui.wait("the badge to hide", |_| ui_braille(&buffer).is_empty())
            .await;
        ui.key(crossterm::event::KeyCode::Enter);
        ui.wait_for("⏺ hello").await;
        assert!(ui_braille(&ui.buffer).is_empty(), "no badge in a session");
    })
    .await;
}
