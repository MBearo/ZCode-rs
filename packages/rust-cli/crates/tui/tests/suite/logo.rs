//! 启动头与新会话的背景标志（docs/specs/rust-cli/tui.md §7.3）：Colophon 在第 3 列；Z 是盲文的点，平时不定时重画，打字时
//! 还在，发出第一条消息就没有；`/clear` 后又画；点在 Z 上播放一种立体动作，结束后回到静止的 Z；16 色不画。
use std::time::Duration;

use crossterm::event::{MouseButton, MouseEventKind};
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::{ColorLevel, Colors};
use rust_cli_tui_kit::render::Color;

use super::support::{GatedModel, Setup, Shot, Ui, with_ui};

/// 盲文字符所在的格子：（列，行）。
fn blocks(shot: &Shot) -> Vec<(u16, u16)> {
    let mut cells = Vec::new();
    for y in shot.area.top()..shot.area.bottom() {
        for x in shot.area.left()..shot.area.right() {
            // 状态行的 spinner 也是盲文：标志不在最后 4 行。
            let braille = shot[(x, y)]
                .symbol()
                .chars()
                .next()
                .is_some_and(|c| ('\u{2801}'..='\u{28ff}').contains(&c));
            if braille && y + 4 < shot.area.bottom() {
                cells.push((x, y));
            }
        }
    }
    cells
}

/// 每个 Z 格子的字符。
fn glyphs(shot: &Shot, cells: &[(u16, u16)]) -> Vec<String> {
    cells
        .iter()
        .map(|&(x, y)| shot[(x, y)].symbol().to_string())
        .collect()
}

fn shown(ui: &Ui) -> Vec<(u16, u16)> {
    ui.shot
        .lock()
        .unwrap()
        .as_ref()
        .map(blocks)
        .unwrap_or_default()
}

fn draws(handovers: &std::sync::Mutex<Vec<&'static str>>) -> usize {
    handovers
        .lock()
        .unwrap()
        .iter()
        .filter(|h| **h == "draw")
        .count()
}

/// 每个 Z 格子的（前景，背景）。
fn tones(shot: &Shot, cells: &[(u16, u16)]) -> Vec<(Color, Color)> {
    cells
        .iter()
        .map(|&(x, y)| (shot[(x, y)].fg, shot[(x, y)].bg))
        .collect()
}

fn setup(background: (u8, u8, u8), foreground: (u8, u8, u8)) -> Setup {
    Setup {
        width: 96,
        height: 28,
        colors: Colors::new(ColorLevel::TrueColor, Some(background))
            .with_foreground(Some(foreground)),
        ..Setup::default()
    }
}

const NIGHT: ((u8, u8, u8), (u8, u8, u8)) = ((0x10, 0x16, 0x1a), (0xe4, 0xdf, 0xd5));
const DAY: ((u8, u8, u8), (u8, u8, u8)) = ((0xf2, 0xf7, 0xf8), (0x22, 0x28, 0x2f));

async fn wait_for_mark(ui: &mut Ui) {
    let shot = ui.shot.clone();
    ui.wait("the mark", move |_| {
        shot.lock()
            .unwrap()
            .as_ref()
            .is_some_and(|s| blocks(s).len() > 20)
    })
    .await;
}

/// Colophon：第 2 行第 3 列的名字（Z 是 brand 色）、下一行的目录；没有 `Type a message`。Z 在下面居中，深浅主题都有；平时不
/// 定时重画。
#[tokio::test(flavor = "multi_thread")]
async fn the_banner_and_the_mark_show_on_a_new_session_and_stay_still() {
    for (background, foreground) in [NIGHT, DAY] {
        let model = GatedModel::scripted(Vec::new());
        with_ui(model, setup(background, foreground), |mut ui| async move {
            wait_for_mark(&mut ui).await;
            let frame = ui.frame();
            let screen = ui.screen();
            let rows: Vec<&str> = screen.lines().collect();
            assert_eq!(rows[0].trim(), "", "{screen}");
            assert!(rows[1].starts_with("  ZCode  0.1.0"), "{screen}");
            assert!(rows[2].starts_with("  ~/"), "{screen}");
            assert_eq!(rows[3].trim(), "", "{screen}");
            assert!(!screen.contains("Type a message"), "{screen}");
            assert_ne!(frame[(2, 1)].fg, frame[(3, 1)].fg, "the Z is brand");
            let cells = blocks(&frame);
            let (left, right) = (
                cells.iter().map(|c| c.0).min().unwrap(),
                cells.iter().map(|c| c.0).max().unwrap(),
            );
            let (top, bottom) = (
                cells.iter().map(|c| c.1).min().unwrap(),
                cells.iter().map(|c| c.1).max().unwrap(),
            );
            assert!(top >= 4 && bottom <= 22, "{top}..{bottom}");
            assert!(left.abs_diff(95 - right) <= 2, "{left}..{right}");
            let before = draws(&ui.handovers);
            tokio::time::sleep(Duration::from_millis(400)).await;
            assert_eq!(draws(&ui.handovers), before, "no redraw while idle");
        })
        .await;
    }
}

/// 打字时还在；发出第一条消息就没有；`/clear` 回到新会话时又画。
#[tokio::test(flavor = "multi_thread")]
async fn the_mark_stays_while_typing_and_goes_on_send() {
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    with_ui(model, setup(NIGHT.0, NIGHT.1), |mut ui| async move {
        wait_for_mark(&mut ui).await;
        let at_start = shown(&ui);
        ui.type_text("hi");
        ui.wait_for("hi").await;
        assert_eq!(shown(&ui), at_start, "typing keeps the mark");
        ui.key(crossterm::event::KeyCode::Enter);
        ui.wait_for("   hello").await;
        // Z 的中部没有了（上部是消息）。
        let middle = at_start[at_start.len() / 2].1;
        assert!(
            shown(&ui).iter().all(|cell| cell.1 < middle),
            "a sent message removes the mark"
        );
        ui.submit("/clear");
        wait_for_mark(&mut ui).await;
    })
    .await;
}

/// 点在 Z 上：按下去（点与颜色都变），弹回、亮度落回之后回到完全相同的静止画面并停止重画；点在空白处什么都没有。
#[tokio::test(flavor = "multi_thread")]
async fn a_click_on_the_z_presses_it_and_settles() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, setup(NIGHT.0, NIGHT.1), |mut ui| async move {
        wait_for_mark(&mut ui).await;
        let frame = ui.frame();
        let cells = blocks(&frame);
        let rest = (glyphs(&frame, &cells), tones(&frame, &cells));
        let press = MouseEventKind::Down(MouseButton::Left);
        // 空白处（Z 的外框之外）：没有动画。
        let before = draws(&ui.handovers);
        ui.mouse(press, 1, 12);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(draws(&ui.handovers) <= before + 1, "no animation");
        // Z 的中间：斜杠上。
        let (x, y) = cells[cells.len() / 2];
        ui.mouse(press, x, y);
        let (shot, wanted, watched) = (ui.shot.clone(), rest.clone(), cells.clone());
        ui.wait("the press", move |_| {
            shot.lock().unwrap().as_ref().is_some_and(|s| {
                let now = blocks(s);
                now != watched || (glyphs(s, &now), tones(s, &now)) != wanted
            })
        })
        .await;
        let (shot, wanted, watched) = (ui.shot.clone(), rest.clone(), cells.clone());
        ui.wait("back to rest", move |_| {
            shot.lock().unwrap().as_ref().is_some_and(|s| {
                blocks(s) == watched && (glyphs(s, &watched), tones(s, &watched)) == wanted
            })
        })
        .await;
        // 最后一帧之后停止重画（给还在路上的那一帧一点时间）。
        tokio::time::sleep(Duration::from_millis(100)).await;
        let settled = draws(&ui.handovers);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(draws(&ui.handovers), settled, "no redraw after the press");
    })
    .await;
}

/// 16 色终端不画 Z，名字还在。
#[tokio::test(flavor = "multi_thread")]
async fn sixteen_colours_have_no_mark() {
    let setup = Setup {
        width: 96,
        height: 28,
        colors: Colors::new(ColorLevel::Ansi16, None),
        ..Setup::default()
    };
    with_ui(
        GatedModel::scripted(Vec::new()),
        setup,
        |mut ui| async move {
            ui.wait_for("ZCode  0.1.0").await;
            assert!(shown(&ui).is_empty());
        },
    )
    .await;
}
