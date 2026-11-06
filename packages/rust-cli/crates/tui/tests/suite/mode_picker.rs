//! 权限模式照 App（docs/specs/rust-cli/tui.md §7、tui-display.md §4.3，工作线 F76，用户决定 2026-10-23）：`/mode`
//! 只列变更前确认 / 自主模式 / 完全访问三个，名称与说明是 App 的 i18n 原文，按界面语言切换；edit 不再能选；Node 写的
//! edit 会话照旧显示原名。模式的颜色照 App：完全访问是 warning 黄、自主模式是 icon-blue 蓝、变更前确认是普通文字色，
//! 深浅两种主题。旧代码的选择器列 plan / build / guarded / yolo 四个 Node 名、footer 的完全访问是红色，这些测试都失败。
use super::support::Shot;
use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_tui::{ColorLevel, Colors, Ink};
use rust_cli_tui_kit::render::{Color, attr};

use super::support::{GatedModel, Setup, Ui, row_of, with_ui};

type Rgb = (u8, u8, u8);

/// 深浅两种背景。
struct Theme {
    background: Rgb,
    foreground: Rgb,
}

const THEMES: [Theme; 2] = [
    Theme {
        background: (0, 0, 0),
        foreground: (255, 255, 255),
    },
    Theme {
        background: (255, 255, 255),
        foreground: (0, 0, 0),
    },
];

/// 一种背景下两个模式的颜色：自主模式是 `Ink::icon_blue`（`brand`），完全访问是 `Ink::warning`（`code`）
/// （tui-kit.md §3.4），期望值经桥求。
fn mode_colors(theme: &Theme) -> (Color, Color) {
    let ink = Ink::new(
        ColorLevel::TrueColor,
        Some(theme.foreground),
        Some(theme.background),
    );
    let color = |style: rust_cli_tui_kit::render::Style| style.fg_color();
    (color(ink.icon_blue()), color(ink.warning()))
}

fn setup(theme: &Theme) -> Setup {
    Setup {
        width: 90,
        height: 30,
        colors: Colors::new(ColorLevel::TrueColor, Some(theme.background))
            .with_foreground(Some(theme.foreground)),
        ..Setup::default()
    }
}

fn frame(ui: &Ui) -> Shot {
    ui.frame()
}

/// 第一处 `needle` 所在的（列，行）。
fn find(buffer: &Shot, needle: &str) -> Option<(u16, u16)> {
    buffer.find(needle)
}

/// 状态栏里模式名称的第一个字（名称后面是 `⇧⇥`，tui-kit.md §4.2「状态栏」）。
fn footer_name(ui: &Ui, name: &str) -> super::support::ShotCell {
    let buffer = frame(ui);
    let (x, y) =
        find(&buffer, &format!("{name} ⇧⇥")).unwrap_or_else(|| panic!("no {name} in the footer"));
    buffer[(x, y)].clone()
}

/// 等状态栏里模式的名字淡入到 `want`（换了模式时它从这一刻淡入，照 Puffin 的 `mode_at`，status_motion.rs）；10 s 内不到时
/// 报出最后看到的颜色。
async fn landed(ui: &Ui, name: &str, want: Color) {
    let wait = async {
        loop {
            if footer_name(ui, name).fg == want {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), wait)
        .await
        .is_err()
    {
        panic!("{name}: {:?}, want {want:?}", footer_name(ui, name).fg);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_picker_lists_the_three_app_modes_with_their_descriptions() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, setup(&THEMES[0]), |mut ui| async move {
        ui.wait_for("Ask before changes ⇧⇥").await;
        ui.submit("/mode");
        let screen = ui
            .wait_for("Asks only before commands that can destroy work")
            .await;
        // 照 Puffin 的模式列表（kit `modes::MODE_ROWS`，tui-kit.md §4.2）：名称与说明在同一行，当前模式后面 `current`，
        // 计划模式是一行开关（`off`）。
        for (label, description, tag) in [
            (
                "Ask before changes",
                "Asks before each file change and command",
                "current",
            ),
            (
                "Autonomous",
                "Asks only before commands that can destroy work",
                "",
            ),
            (
                "Full access",
                "Runs commands and edits files without asking",
                "",
            ),
            (
                "Plan mode",
                "Reads and plans; changes nothing until approved",
                "off",
            ),
        ] {
            let label_row = row_of(&screen, &format!("   {label}  "))
                .unwrap_or_else(|| panic!("{label}\n{screen}"));
            assert_eq!(row_of(&screen, description), Some(label_row), "{screen}");
            let line = screen.lines().nth(usize::from(label_row)).unwrap();
            assert!(
                line.trim_end().ends_with(description) || line.trim_end().ends_with(tag),
                "{screen}"
            );
            assert_eq!(line.contains("current"), tag == "current", "{screen}");
        }
        for gone in ["Edit automatically", "4. "] {
            assert!(!screen.contains(gone), "{gone}\n{screen}");
        }
        // 数字键直接选（完全访问先确认，工作线 F87）；确认之后不另说一句（Puffin），状态栏的模式跟着变。
        ui.key(KeyCode::Char('3'));
        ui.confirm_full_access().await;
        ui.wait_for("Full access ⇧⇥").await;
        // edit 不再是选项；带参数照旧能切。
        ui.submit("/mode edit");
        ui.wait_for("Unsupported mode: edit. Available modes: build, guarded, yolo.")
            .await;
        ui.submit("/mode guarded");
        let screen = ui
            .wait_for("Autonomous: it asks only before commands that can destroy work.")
            .await;
        assert!(screen.contains("Autonomous ⇧⇥"), "{screen}");
    })
    .await;
    // 中文：App 的中文名称与说明。
    let model = GatedModel::scripted(Vec::new());
    let zh = Setup {
        lang: "zh_CN.UTF-8",
        ..setup(&THEMES[0])
    };
    with_ui(model, zh, |mut ui| async move {
        ui.wait_for("变更前确认 ⇧⇥").await;
        ui.submit("/mode");
        let screen = ui.wait_for("只在可能毁掉工作的命令前问").await;
        for text in [
            "变更前确认",
            "每次改文件、跑命令前都先问",
            "自主模式",
            "完全访问",
            "跑命令、改文件都不问",
            "计划模式",
        ] {
            assert!(screen.contains(text), "{text}\n{screen}");
        }
        let current = screen
            .lines()
            .find(|l| l.contains("每次改文件、跑命令前都先问"))
            .unwrap();
        assert!(current.contains("当前"), "{screen}");
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("自主：只在可能毁掉工作的命令前问。").await;
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
        ui.wait_for("Edit automatically ⇧⇥").await;
        ui.submit("/mode");
        let screen = ui
            .wait_for("Asks only before commands that can destroy work")
            .await;
        let rows: Vec<&str> = screen
            .lines()
            .filter(|l| l.contains("Ask before file changes.") || l.contains("Run with fewer"))
            .collect();
        assert!(rows.iter().all(|l| !l.contains("current")), "{screen}");
        assert!(!screen.contains("   Edit automatically   "), "{screen}");
        ui.key(KeyCode::Esc);
        ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
        ui.wait_for("Ask before changes ⇧⇥").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mode_names_use_the_app_colors_in_both_themes() {
    for theme in &THEMES {
        let (guarded, full) = mode_colors(theme);
        let model = GatedModel::scripted(Vec::new());
        with_ui(model, setup(theme), |mut ui| async move {
            ui.wait_for("Ask before changes ⇧⇥").await;
            // 变更前确认：fg3（Puffin `mode_style`，tui-kit.md §4.2「状态栏」），不 dim。
            let build = footer_name(&ui, "Ask before changes");
            let quiet = Ink::new(
                ColorLevel::TrueColor,
                Some(theme.foreground),
                Some(theme.background),
            )
            .theme
            .fg3
            .fg_color();
            assert_eq!(build.fg, quiet, "{build:?}");
            assert!(!build.has(attr::DIM), "{build:?}");
            ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
            ui.wait_for("Autonomous ⇧⇥").await;
            landed(&ui, "Autonomous", guarded).await;
            ui.press(KeyCode::BackTab, KeyModifiers::SHIFT);
            ui.confirm_full_access().await;
            ui.wait_for("Full access ⇧⇥").await;
            landed(&ui, "Full access", full).await;
            // 选择器照 Puffin 的模式列表（tui-kit.md §4.2、§6）：名字不按模式上色，选中的 `Ink::selection`、其余 fg2。
            let ink = Ink::new(
                ColorLevel::TrueColor,
                Some(theme.foreground),
                Some(theme.background),
            );
            let chosen = ink.selection().fg_color();
            let other = ink.theme.fg2.fg_color();
            ui.submit("/mode");
            ui.wait_for("Asks only before commands that can destroy work")
                .await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "   Full access   ").unwrap();
            assert_eq!(buffer[(x + 3, y)].fg, chosen);
            let (x, y) = find(&buffer, "   Autonomous   ").unwrap();
            assert_eq!(buffer[(x + 3, y)].fg, other);
            ui.key(KeyCode::Up);
            let moved = async {
                loop {
                    let buffer = frame(&ui);
                    if find(&buffer, "   Autonomous   ")
                        .is_some_and(|(x, y)| buffer[(x + 3, y)].fg == chosen)
                    {
                        return;
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
            };
            tokio::time::timeout(std::time::Duration::from_secs(10), moved)
                .await
                .expect("the choice moves to guarded");
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "   Full access   ").unwrap();
            assert_eq!(buffer[(x + 3, y)].fg, other);
        })
        .await;
    }
}
