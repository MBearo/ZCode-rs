//! 权限模式照 App（docs/specs/rust-cli/tui.md §7、tui-display.md §4.3，工作线 F76，用户决定 2026-10-23）：`/mode`
//! 只列变更前确认 / 自主模式 / 完全访问三个，名称与说明是 App 的 i18n 原文，按界面语言切换；edit 不再能选；Node 写的
//! edit 会话照旧显示原名。模式的颜色照 App：完全访问是 warning 黄、自主模式是 icon-blue 蓝、变更前确认是普通文字色，
//! 深浅两种主题。旧代码的选择器列 plan / build / guarded / yolo 四个 Node 名、footer 的完全访问是红色，这些测试都失败。
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_tui::ui::palette::{ColorLevel, Palette};

use super::support::{GatedModel, Setup, Ui, row_of, with_ui};

type Rgb = (u8, u8, u8);

/// 一种背景下 App 的两个主题色（`packages/ui/src/styles.css` 的 `--color-icon-blue` 与 `--color-warning`）。
struct Theme {
    background: Rgb,
    foreground: Rgb,
    guarded: Color,
    full: Color,
}

const THEMES: [Theme; 2] = [
    // 深色：sky-600、yellow-500。
    Theme {
        background: (0, 0, 0),
        foreground: (255, 255, 255),
        guarded: Color::Rgb(0, 132, 209),
        full: Color::Rgb(240, 177, 0),
    },
    // 浅色：sky-500、yellow-600。
    Theme {
        background: (255, 255, 255),
        foreground: (0, 0, 0),
        guarded: Color::Rgb(0, 166, 244),
        full: Color::Rgb(208, 135, 0),
    },
];

fn setup(theme: &Theme) -> Setup {
    Setup {
        width: 90,
        height: 30,
        palette: Palette::new(ColorLevel::TrueColor, Some(theme.background))
            .with_foreground(Some(theme.foreground)),
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

/// footer 里模式名称的第一个字。
fn footer_name(ui: &Ui, name: &str) -> ratatui::buffer::Cell {
    let buffer = frame(ui);
    let (x, y) = find(&buffer, &format!("⏵⏵ {name}"))
        .or_else(|| find(&buffer, &format!("⏸ {name}")))
        .unwrap_or_else(|| panic!("no {name} in the footer"));
    buffer[(x + 3, y)].clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_picker_lists_the_three_app_modes_with_their_descriptions() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, setup(&THEMES[0]), |mut ui| async move {
        ui.wait_for("⏸ Ask before changes").await;
        ui.submit("/mode");
        let screen = ui.wait_for("Run with fewer confirmations.").await;
        // 名称一行、说明在下一行（App 菜单的排法），当前模式打勾。
        for (label, description) in [
            ("1. Ask before changes ✔", "Ask before file changes."),
            ("2. Autonomous mode", "Ask when there’s risk"),
            ("3. Full access", "Run with fewer confirmations."),
        ] {
            let label_row = row_of(&screen, label).unwrap_or_else(|| panic!("{label}\n{screen}"));
            assert_eq!(
                row_of(&screen, description),
                Some(label_row + 1),
                "{screen}"
            );
        }
        for gone in ["Edit automatically", "Plan mode", "4. "] {
            assert!(!screen.contains(gone), "{gone}\n{screen}");
        }
        // 数字键直接选（完全访问先确认，工作线 F87）；提示用 App 的名称。
        ui.key(KeyCode::Char('3'));
        ui.confirm_full_access().await;
        let screen = ui.wait_for("Mode switched to Full access.").await;
        assert!(screen.contains("⏵⏵ Full access"), "{screen}");
        // edit 不再是选项；带参数照旧能切。
        ui.submit("/mode edit");
        ui.wait_for("Unsupported mode: edit. Available modes: build, guarded, yolo.")
            .await;
        ui.submit("/mode guarded");
        let screen = ui.wait_for("Mode switched to Autonomous mode.").await;
        assert!(screen.contains("⏵⏵ Autonomous mode"), "{screen}");
    })
    .await;
    // 中文：App 的中文名称与说明。
    let model = GatedModel::scripted(Vec::new());
    let zh = Setup {
        lang: "zh_CN.UTF-8",
        ..setup(&THEMES[0])
    };
    with_ui(model, zh, |mut ui| async move {
        ui.wait_for("⏸ 变更前确认").await;
        ui.submit("/mode");
        let screen = ui.wait_for("减少确认次数。").await;
        for text in [
            "1. 变更前确认 ✔",
            "改文件前先问我。",
            "2. 自主模式",
            "在有风险时询问",
            "3. 完全访问",
        ] {
            assert!(screen.contains(text), "{text}\n{screen}");
        }
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("模式已切换为 自主模式。").await;
    })
    .await;
}

/// Node 写的 edit 会话（或 `--permission-mode acceptEdits`）：footer 照旧是“自动编辑”，选择器里没有它、谁都不打勾；
/// shift+tab 回到变更前确认。
#[tokio::test(flavor = "multi_thread")]
async fn an_edit_session_keeps_its_name_but_is_not_offered() {
    let model = GatedModel::scripted(Vec::new());
    let edit = Setup {
        mode: PermissionMode::AcceptEdits,
        ..setup(&THEMES[0])
    };
    with_ui(model, edit, |mut ui| async move {
        ui.wait_for("⏵⏵ Edit automatically").await;
        ui.submit("/mode");
        let screen = ui.wait_for("3. Full access").await;
        assert!(!screen.contains('✔'), "{screen}");
        assert!(!screen.contains("2. Edit automatically"), "{screen}");
        ui.key(KeyCode::Esc);
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.wait_for("⏸ Ask before changes").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mode_names_use_the_app_colors_in_both_themes() {
    for theme in &THEMES {
        let (guarded, full) = (theme.guarded, theme.full);
        let model = GatedModel::scripted(Vec::new());
        with_ui(model, setup(theme), |mut ui| async move {
            ui.wait_for("⏸ Ask before changes").await;
            // 变更前确认：普通文字色，不 dim。
            let build = footer_name(&ui, "Ask before changes");
            assert_eq!(build.fg, Color::Reset, "{build:?}");
            assert!(!build.modifier.contains(Modifier::DIM), "{build:?}");
            ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
            ui.wait_for("⏵⏵ Autonomous mode").await;
            assert_eq!(footer_name(&ui, "Autonomous mode").fg, guarded);
            ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
            ui.confirm_full_access().await;
            ui.wait_for("⏵⏵ Full access").await;
            assert_eq!(footer_name(&ui, "Full access").fg, full);
            // 选择器里没选中的行同样上色（选中行是选择列表的底色条）。
            ui.submit("/mode");
            ui.wait("the picker", |s| s.contains("› 3. Full access"))
                .await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "2. Autonomous mode").unwrap();
            assert_eq!(buffer[(x + 3, y)].fg, guarded);
            let (x, y) = find(&buffer, "1. Ask before changes").unwrap();
            assert_eq!(buffer[(x + 3, y)].fg, Color::Reset);
            ui.key(KeyCode::Up);
            ui.wait("the pointer on guarded", |s| {
                s.contains("› 2. Autonomous mode")
            })
            .await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "3. Full access").unwrap();
            assert_eq!(buffer[(x + 3, y)].fg, full);
        })
        .await;
    }
}
