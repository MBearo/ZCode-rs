//! 配色与共用组件（docs/specs/rust-cli/tui.md §7.4，修复线 F26，用户决定）：选中行的
//! 底色条与 `›`、按键提示的按键与说明、快捷键面板在输入框上面、模型名与路径的颜色；深浅两种背景、中英文。
//! 旧代码的选中行是强调色的 `❯ `、提示整行 dim、快捷键面板在输入框下面，这些测试都失败。
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use rust_cli_tui::ui::palette::{ColorLevel, Palette};

use super::support::{GatedModel, Setup, Ui, with_ui};

const DARK: (u8, u8, u8) = (0, 0, 0);
const LIGHT: (u8, u8, u8) = (255, 255, 255);

fn setup(background: (u8, u8, u8), foreground: (u8, u8, u8), lang: &'static str) -> Setup {
    Setup {
        width: 100,
        height: 30,
        lang,
        palette: Palette::new(ColorLevel::TrueColor, Some(background))
            .with_foreground(Some(foreground)),
        ..Setup::default()
    }
}

fn frame(ui: &Ui) -> Buffer {
    ui.buffer.lock().unwrap().clone().unwrap()
}

/// 第一处 `needle` 所在的（列，行）。
fn find(buffer: &Buffer, needle: &str) -> Option<(u16, u16)> {
    let area = buffer.area;
    (area.top()..area.bottom()).find_map(|y| {
        let cells: Vec<(u16, String)> = (area.left()..area.right())
            .map(|x| (x, buffer[(x, y)].symbol().to_owned()))
            .collect();
        let row: String = cells.iter().map(|(_, s)| s.as_str()).collect();
        let at = row.find(needle)?;
        // 字节位置换成列：数前面的格子。
        let mut bytes = 0;
        cells
            .iter()
            .find(|(_, s)| {
                let hit = bytes >= at;
                bytes += s.len();
                hit
            })
            .map(|(x, _)| (*x, y))
    })
}

/// 选中行：`› ` 开头，整行铺底色条（行尾的格子也是），名字粗体。`row` 是选中行里的文字，`name` 是其中的名字。
async fn assert_selected_bar(ui: &mut Ui, row: &str, name: &str, bar: Color) {
    let wanted = format!("› {row}");
    ui.wait(&wanted.clone(), move |s| s.contains(&wanted)).await;
    let buffer = frame(ui);
    let (x, y) = find(&buffer, &format!("› {row}")).unwrap();
    let offset = row.find(name).unwrap() as u16;
    let cell = &buffer[(x + 2 + offset, y)];
    assert_eq!(cell.bg, bar, "the bar under the name");
    assert!(cell.modifier.contains(Modifier::BOLD), "the name is bold");
    let last = &buffer[(buffer.area.right() - 1, y)];
    assert_eq!(last.bg, bar, "the bar reaches the end of the row");
}

#[tokio::test(flavor = "multi_thread")]
async fn pickers_and_popups_share_the_selection_bar_in_both_themes() {
    for (background, foreground, bar) in [
        (DARK, LIGHT, Color::Rgb(99, 168, 248)),
        (LIGHT, DARK, Color::Rgb(164, 205, 251)),
    ] {
        let model = GatedModel::scripted(Vec::new());
        with_ui(
            model,
            setup(background, foreground, "en_US.UTF-8"),
            |mut ui| async move {
                ui.wait_for("Ask before changes").await;
                // `/mode` 的选择器（通用列表；工作线 F76 起是 App 的三个模式，当前的变更前确认在第 1 行）。
                ui.submit("/mode");
                assert_selected_bar(&mut ui, "1. Ask before changes", "Ask before changes", bar)
                    .await;
                ui.key(crossterm::event::KeyCode::Esc);
                // 斜杠命令的弹出列表。
                ui.type_text("/he");
                assert_selected_bar(&mut ui, "/help", "/help", bar).await;
            },
        )
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn key_hints_are_bold_keys_with_gray_labels() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model,
        setup(DARK, LIGHT, "en_US.UTF-8"),
        |mut ui| async move {
            ui.wait_for("? for shortcuts").await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "? for shortcuts").unwrap();
            let key = &buffer[(x, y)];
            assert!(key.modifier.contains(Modifier::BOLD), "{key:?}");
            assert!(!key.modifier.contains(Modifier::DIM), "{key:?}");
            let label = &buffer[(x + 2, y)];
            assert_eq!(label.fg, Color::Rgb(153, 153, 153), "{label:?}");
            assert!(!label.modifier.contains(Modifier::BOLD), "{label:?}");
            // 列表底部的提示也一样。
            ui.submit("/mode");
            ui.wait_for("Enter to select · Esc to cancel").await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "Enter to select").unwrap();
            assert!(buffer[(x, y)].modifier.contains(Modifier::BOLD));
            assert_eq!(buffer[(x + 6, y)].fg, Color::Rgb(153, 153, 153));
        },
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_shortcuts_panel_sits_above_the_input_box() {
    for (lang, title, group, close) in [
        (
            "en_US.UTF-8",
            "Keyboard shortcuts",
            "Transcript",
            "esc to close",
        ),
        ("zh_CN.UTF-8", "快捷键", "记录", "esc 关闭"),
    ] {
        let model = GatedModel::scripted(Vec::new());
        with_ui(model, setup(DARK, LIGHT, lang), |mut ui| async move {
            // footer 就绪：权限模式的符号（权限模式后面不再有 `shift+tab` 提示，2026-10-20）。
            ui.wait("the footer", |s| s.contains("⏸")).await;
            ui.key(crossterm::event::KeyCode::Char('?'));
            // 等关闭的提示（footer 平时的 `? for shortcuts` 里也有“快捷键”）。
            let screen = ui.wait_for(close).await;
            assert!(screen.contains(group), "{screen}");
            assert!(screen.contains(title), "{screen}");
            let lines: Vec<&str> = screen.lines().collect();
            let panel = lines.iter().position(|l| l.trim() == title).unwrap();
            let input = lines.iter().position(|l| l.starts_with("❯")).unwrap();
            assert!(panel < input, "the panel is above the input box:\n{screen}");
            // 三组并排：组名在同一行。
            let groups = lines[panel + 2];
            assert!(groups.contains(group), "{screen}");
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "shift+enter").unwrap();
            assert!(
                buffer[(x, y)].modifier.contains(Modifier::BOLD),
                "keys are bold"
            );
        })
        .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_model_and_the_path_use_the_status_line_colors() {
    for (background, foreground, model_rgb) in [
        (DARK, LIGHT, Color::Rgb(246, 226, 183)),
        ((30, 30, 46), (205, 214, 244), Color::Rgb(246, 226, 183)),
    ] {
        let model = GatedModel::scripted(Vec::new());
        with_ui(
            model,
            setup(background, foreground, "en_US.UTF-8"),
            |mut ui| async move {
                let screen = ui.wait_for("scripted · ~/project").await;
                assert!(screen.contains("scripted"), "{screen}");
                let buffer = frame(&ui);
                let (x, y) = find(&buffer, "scripted · ~/project").unwrap();
                assert_eq!(buffer[(x, y)].fg, model_rgb, "the model in the header");
                let (px, py) = find(&buffer, "~/project").unwrap();
                assert_eq!(buffer[(px, py)].fg, Color::Rgb(171, 223, 167), "the path");
            },
        )
        .await;
    }
    // 浅色背景：颜色按对比度调深，不同于深色背景的取值。
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model,
        setup(LIGHT, DARK, "en_US.UTF-8"),
        |mut ui| async move {
            ui.wait_for("scripted · ~/project").await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "scripted · ~/project").unwrap();
            assert_ne!(buffer[(x, y)].fg, Color::Rgb(246, 226, 183));
            assert!(matches!(buffer[(x, y)].fg, Color::Rgb(..)));
        },
    )
    .await;
}
