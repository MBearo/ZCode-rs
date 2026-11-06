//! 配色与共用组件（docs/specs/rust-cli/tui.md §7.4，修复线 F26，用户决定）：选中行的
//! 底色条与 `›`、按键提示的按键与说明、快捷键面板在输入框上面、模型名与路径的颜色；深浅两种背景、中英文。
//! 旧代码的选中行是强调色的 `❯ `、提示整行 dim、快捷键面板在输入框下面，这些测试都失败。
use super::support::Shot;
use rust_cli_tui::{ColorLevel, Colors, Ink};
use rust_cli_tui_kit::render::{Color, attr};

use super::support::{GatedModel, Setup, Ui, with_ui};

const DARK: (u8, u8, u8) = (0, 0, 0);
const LIGHT: (u8, u8, u8) = (255, 255, 255);

fn setup(background: (u8, u8, u8), foreground: (u8, u8, u8), lang: &'static str) -> Setup {
    Setup {
        width: 100,
        height: 30,
        lang,
        colors: Colors::new(ColorLevel::TrueColor, Some(background))
            .with_foreground(Some(foreground)),
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

/// 选中的条目（照 Puffin 的层，tui-kit.md §3.4、§4.2）：没有 `›` 与底色条，名字取 `Ink::selection`（rich 是 brand 的字，
/// plain 再加粗）；底色与板的同一行一样（rich 是 panel，plain 是终端底）。`name` 是条目的名字，从第 3 列起、后面隔三格。
async fn assert_chosen(ui: &mut Ui, name: &str, ink: Ink) {
    let wanted = format!("   {name}   ");
    ui.wait(&wanted.clone(), move |s| s.contains(&wanted)).await;
    let buffer = frame(ui);
    let (x, y) = find(&buffer, &format!("   {name}   ")).unwrap();
    let cell = &buffer[(x + 3, y)];
    let selection = ink.selection();
    assert_eq!(
        cell.fg,
        selection.fg_color(),
        "the selection ink on the name"
    );
    assert_eq!(
        cell.has(attr::BOLD),
        selection.has_attr(attr::BOLD),
        "bold only in plain"
    );
    assert_eq!(cell.bg, buffer[(0, y)].bg, "no bar: the layer's ground");
}

#[tokio::test(flavor = "multi_thread")]
async fn pickers_and_popups_share_the_chosen_ink_in_both_themes() {
    for (background, foreground) in [(DARK, LIGHT), (LIGHT, DARK)] {
        let bar = Ink::new(ColorLevel::TrueColor, Some(foreground), Some(background));
        let model = GatedModel::scripted(Vec::new());
        with_ui(
            model,
            setup(background, foreground, "en_US.UTF-8"),
            |mut ui| async move {
                ui.wait_for("Ask before changes").await;
                // `/mode` 的选择器（通用列表；工作线 F76 起是 App 的三个模式，当前的变更前确认在第 1 行，打开时选中它）。
                ui.submit("/mode");
                assert_chosen(&mut ui, "Ask before changes", bar).await;
                ui.key(crossterm::event::KeyCode::Esc);
                // esc 留着草稿 `/mode `（tui-kit.md §4.0「板」）：清掉再打。
                ui.clear();
                // 斜杠命令的弹出列表：S2 起是 Puffin 的命令架（tui-kit.md §4.2「斜杠命令」），没有底色条——选中的名字
                // brand、描述 fg2。
                // `/he` 列出 `/help` 所指的 `/keys`（命令架只写命令名，Puffin `shelf::matches`）。
                ui.type_text("/ke");
                ui.wait_for("/keys").await;
                let buffer = frame(&ui);
                let (x, y) = find(&buffer, "/keys").unwrap();
                assert_eq!(buffer[(x, y)].fg, bar.theme.brand, "the chosen name");
                let desc = (x + 5..buffer.area.right())
                    .find(|&c| buffer[(c, y)].symbol() != " ")
                    .unwrap();
                assert_eq!(
                    buffer[(desc, y)].fg,
                    bar.theme.fg2.fg_color(),
                    "its description"
                );
            },
        )
        .await;
    }
}

/// 按键提示照 Puffin 的槽（tui-kit.md §3.8 的转换示例，docs/puffin-tui.md §4.2「按键提示（槽）」）：键 `fg2`、标签 `fg3`。
/// 以前是粗体的键与 (153,153,153) 的说明（修复线 F26）。
#[tokio::test(flavor = "multi_thread")]
async fn key_hints_take_the_puffin_slot_inks() {
    let ink = Ink::new(ColorLevel::TrueColor, Some(LIGHT), Some(DARK));
    let key_fg = Some(ink.theme.fg2.fg_color());
    let label_fg = Some(ink.theme.fg3.fg_color());
    assert_ne!(key_fg, label_fg);
    let model = GatedModel::scripted(Vec::new());
    with_ui(
        model,
        setup(DARK, LIGHT, "en_US.UTF-8"),
        |mut ui| async move {
            ui.wait_for("? keys").await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "? keys").unwrap();
            let key = &buffer[(x, y)];
            assert_eq!(Some(key.fg), key_fg, "{key:?}");
            assert!(!key.has(attr::BOLD), "{key:?}");
            let label = &buffer[(x + 2, y)];
            assert_eq!(Some(label.fg), label_fg, "{label:?}");
            // 层的槽也一样：模型选择器的 `←→ effort`（Puffin `picker_open` 的槽）；槽变了时淡入，等它画完。
            ui.submit("/model");
            ui.wait_for("←→ effort").await;
            ui.settle().await;
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "←→ effort").unwrap();
            assert_eq!(Some(buffer[(x, y)].fg), key_fg);
            assert_eq!(Some(buffer[(x + 3, y)].fg), label_fg);
        },
    )
    .await;
}

/// `?` 按键表（S2 起照 Puffin 的按键表，kit `key_hints::sheet_of`，tui-kit.md §4.2「? 面板」）：在输入框的板上、输入行
/// 之上，三组并排、组名在同一行；不再写「Keyboard shortcuts」的标题（§6）。打出的 `?` 在草稿里，槽空着（Puffin
/// `sheet_open`，tui-kit.md §4.0「板」）；以前 `?` 不进草稿，槽写 `esc to close`。
#[tokio::test(flavor = "multi_thread")]
async fn the_shortcuts_panel_sits_above_the_input_box() {
    for (lang, mode, group) in [
        ("en_US.UTF-8", "Ask before changes", "Anywhere"),
        ("zh_CN.UTF-8", "变更前确认", "任何时候"),
    ] {
        let model = GatedModel::scripted(Vec::new());
        with_ui(model, setup(DARK, LIGHT, lang), |mut ui| async move {
            ui.wait("the status bar", |s| s.contains(mode)).await;
            ui.key(crossterm::event::KeyCode::Char('?'));
            let screen = ui.wait_for(group).await;
            let lines: Vec<&str> = screen.lines().collect();
            let groups = lines.iter().position(|l| l.contains(group)).unwrap();
            let input = lines.iter().position(|l| l.starts_with(" ›")).unwrap();
            assert!(
                groups < input,
                "the sheet is above the input row:\n{screen}"
            );
            assert_eq!(lines[input].trim_end(), " › ?", "{screen}");
            let buffer = frame(&ui);
            let (x, y) = find(&buffer, "⇧⏎").unwrap();
            // 按键用 `Ink::key`（Puffin 按键提示的键：`fg2`，tui-kit.md §3.4），期望值经桥求。
            let key = Ink::new(ColorLevel::TrueColor, Some(LIGHT), Some(DARK)).key();
            assert_eq!(
                Some(buffer[(x, y)].fg),
                Some(key.fg_color()),
                "keys use the key ink"
            );
        })
        .await;
    }
}

/// 启动头是 Puffin 的欢迎块（tui-kit.md §4.2「logo」）：`ZCode` 粗体、版本 fg3、目录 fg2（期望值经 `Ink` 求）；浅色背景时按
/// 对比度调深，不同于深色背景的取值。
#[tokio::test(flavor = "multi_thread")]
async fn the_welcome_block_uses_the_puffin_inks() {
    let ink_of = |background, foreground| {
        let ink = Ink::new(ColorLevel::TrueColor, Some(foreground), Some(background));
        (ink.theme.fg2.fg_color(), ink.theme.fg3.fg_color())
    };
    let (dark_path, _) = ink_of(DARK, LIGHT);
    for (background, foreground) in [
        (DARK, LIGHT),
        ((30, 30, 46), (205, 214, 244)),
        (LIGHT, DARK),
    ] {
        let (fg2, fg3) = ink_of(background, foreground);
        assert_ne!(fg2, fg3, "fg2 and fg3 differ");
        let model = GatedModel::scripted(Vec::new());
        with_ui(
            model,
            setup(background, foreground, "en_US.UTF-8"),
            |mut ui| async move {
                ui.wait_for("ZCode  0.1.0").await;
                let buffer = frame(&ui);
                let (x, y) = find(&buffer, "ZCode  0.1.0").unwrap();
                assert!(buffer[(x, y)].has(attr::BOLD), "ZCode is bold");
                assert_eq!(buffer[(x + 7, y)].fg, fg3, "the version");
                assert_eq!(buffer[(x, y + 1)].fg, fg2, "the folder");
                assert!(matches!(buffer[(x, y + 1)].fg, Color::TrueColor(..)));
            },
        )
        .await;
    }
    assert_ne!(ink_of(LIGHT, DARK).0, dark_path, "light backgrounds darken");
}
