//! 深浅主题与中英文（docs/specs/rust-cli/tui.md §7）：配色按终端背景派生，文字按 `LANG`。
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::{ColorLevel, Colors, Ink};
use rust_cli_tui_kit::render::Color;

use super::support::{GatedModel, Setup, with_ui};

/// 画面里第一个以 `prefix` 开头的行的第一个格子的背景色。
fn row_background(ui: &super::support::Ui, prefix: &str) -> Option<Color> {
    let buffer = ui.shot.lock().unwrap().clone()?;
    let area = buffer.area;
    (area.top()..area.bottom()).find_map(|y| {
        let row: String = (area.left()..area.right())
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        row.starts_with(prefix).then(|| buffer[(area.left(), y)].bg)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn light_and_dark_backgrounds_shade_the_user_message_differently() {
    // 用户消息的板是主题的 `panel`（tui-kit.md §3.4，`Ink::prompt_bg`）：报告了前景与背景时是 rich 的主题，期望值经桥求。
    let mut shades = Vec::new();
    let mut panels = Vec::new();
    for (background, foreground) in [((0, 0, 0), (255, 255, 255)), ((255, 255, 255), (0, 0, 0))] {
        let model = GatedModel::scripted(vec![text_response(&["ok"])]);
        let colors =
            Colors::new(ColorLevel::TrueColor, Some(background)).with_foreground(Some(foreground));
        let panel = colors.ink().prompt_bg().expect("rich has a panel");
        panels.push(panel);
        let setup = Setup {
            colors,
            ..Setup::default()
        };
        let shade = std::sync::Arc::new(std::sync::Mutex::new(None));
        let found = shade.clone();
        with_ui(model, setup, |mut ui| async move {
            ui.submit("shade me");
            ui.wait("the reply", |s| s.contains("ok") && !s.contains("esc stop"))
                .await;
            *found.lock().unwrap() = row_background(&ui, " › shade me");
        })
        .await;
        shades.push(shade.lock().unwrap().unwrap());
    }
    assert_eq!(shades[0], panels[0], "dark: the panel");
    assert_eq!(shades[1], panels[1], "light: the panel");
    assert_ne!(shades[0], shades[1], "dark and light differ");
    assert!(
        shades.iter().all(|shade| *shade != Color::Default),
        "{shades:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_chinese_locale_shows_chinese_text() {
    let model = GatedModel::scripted(vec![text_response(&["好的"])]);
    let setup = Setup {
        lang: "zh_CN.UTF-8",
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        let start = ui.wait_for("变更前确认").await;
        // Puffin 的欢迎块没有示例行（以前是 `试试“…”`）。
        assert!(!start.contains("试试“"), "{start}");
        // 用户决定（2026-10-20）：权限模式后面不再有 `（shift+tab 切换）`。
        assert!(!start.contains("shift+tab"), "{start}");
        ui.submit("/nope");
        ui.wait_for("没有命令 '/nope'").await;
        // 不存在的命令留在输入框里。以前接着看 `/status` 页的中文（`权限`、`还没有开始`）；`/status` 现在是 `/stats` 的卡
        // （Puffin，kit 的卡只有英文）。
        assert!(ui.screen().contains("› /nope"));
    })
    .await;
}

/// 内存里的 `theme.json`（tui-kit.md §3.4）：读到的是写好的文字，写入不做事。
struct ThemeFile(String);

impl rust_cli_tui::services::Stores for ThemeFile {
    fn read(
        &self,
        file: rust_cli_tui::services::StoreFile,
    ) -> futures_util::future::BoxFuture<'static, Result<Option<String>, String>> {
        let text = (file == rust_cli_tui::services::StoreFile::Theme).then(|| self.0.clone());
        Box::pin(async move { Ok(text) })
    }

    fn write(
        &self,
        _file: rust_cli_tui::services::StoreFile,
        _text: String,
    ) -> futures_util::future::BoxFuture<'static, Result<(), String>> {
        Box::pin(async { Ok(()) })
    }
}

/// 主题的选择来自 `theme.json`（与 Puffin 同一个文件，tui-kit.md §2.3、§3.4）：启动时读一次，第一帧就是它；用户消息的板是
/// 那个主题的 `panel`，与缺省（Terminal）的不同。
#[tokio::test(flavor = "multi_thread")]
async fn the_theme_comes_from_the_puffin_theme_file() {
    use rust_cli_tui_kit::theme::scheme::ThemeChoice;
    let (background, foreground) = ((0, 0, 0), (255, 255, 255));
    let named = rust_cli_tui_kit::theme::schemes::ids()
        .iter()
        .copied()
        .find(|id| *id != rust_cli_tui_kit::theme::schemes::TERMINAL)
        .expect("a named theme");
    let choice = ThemeChoice {
        theme: named,
        ..ThemeChoice::default()
    };
    let panel = Ink::chosen(
        ColorLevel::TrueColor,
        Some(foreground),
        Some(background),
        choice,
    )
    .prompt_bg()
    .expect("rich has a panel");
    let default = Ink::new(ColorLevel::TrueColor, Some(foreground), Some(background)).prompt_bg();
    assert_ne!(Some(panel), default, "the named theme has its own panel");
    let mut setup = Setup {
        colors: Colors::new(ColorLevel::TrueColor, Some(background))
            .with_foreground(Some(foreground)),
        ..Setup::default()
    };
    setup.services.stores = Some(std::sync::Arc::new(ThemeFile(choice.encode())));
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let shade = std::sync::Arc::new(std::sync::Mutex::new(None));
    let found = shade.clone();
    with_ui(model, setup, |mut ui| async move {
        ui.submit("shade me");
        ui.wait("the reply", |s| s.contains("ok") && !s.contains("esc stop"))
            .await;
        *found.lock().unwrap() = row_background(&ui, " › shade me");
    })
    .await;
    assert_eq!(*shade.lock().unwrap(), Some(panel));
}
