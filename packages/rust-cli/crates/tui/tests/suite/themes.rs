//! 深浅主题与中英文（docs/specs/rust-cli/tui.md §7）：配色按终端背景派生，文字按 `LANG`。
use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::style::Color;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::ui::palette::{ColorLevel, Palette};

use super::support::{GatedModel, Setup, with_ui};

/// 画面里第一个以 `prefix` 开头的行的第一个格子的背景色。
fn row_background(ui: &super::support::Ui, prefix: &str) -> Option<Color> {
    let buffer = ui.buffer.lock().unwrap().clone()?;
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
    let mut shades = Vec::new();
    for background in [(0, 0, 0), (255, 255, 255)] {
        let model = GatedModel::scripted(vec![text_response(&["ok"])]);
        let setup = Setup {
            palette: Palette::new(ColorLevel::TrueColor, Some(background)),
            ..Setup::default()
        };
        let shade = std::sync::Arc::new(std::sync::Mutex::new(None));
        let found = shade.clone();
        with_ui(model, setup, |mut ui| async move {
            ui.submit("shade me");
            ui.wait("the reply", |s| {
                s.contains("ok") && !s.contains("esc to interrupt")
            })
            .await;
            *found.lock().unwrap() = row_background(&ui, "❯ shade me");
        })
        .await;
        shades.push(shade.lock().unwrap().unwrap());
    }
    assert_eq!(
        shades[0],
        Color::Rgb(41, 41, 41),
        "dark: lighter than black"
    );
    assert_eq!(
        shades[1],
        Color::Rgb(250, 250, 250),
        "light: darker than white"
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
        assert!(start.contains("试试“"), "{start}");
        // 用户决定（2026-10-20）：权限模式后面不再有 `（shift+tab 切换）`。
        assert!(!start.contains("shift+tab"), "{start}");
        ui.submit("/nope");
        ui.wait_for("没有命令 '/nope'").await;
        // 不存在的命令留在输入框里：先清掉。
        ui.press(KeyCode::Char('u'), KeyModifiers::CONTROL);
        ui.submit("/status");
        let status = ui.wait_for("权限").await;
        assert!(status.contains("还没有开始"), "{status}");
    })
    .await;
}
