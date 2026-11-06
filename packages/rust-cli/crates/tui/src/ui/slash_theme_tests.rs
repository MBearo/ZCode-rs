//! `/theme` 的列表（tui-kit.md §4.2「`/theme`」）：按键与滚轮移动焦点走同一条预览的路，esc 换回打开时的主题，⏎ 交给
//! runner 写 `theme.json`。

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::theme::schemes;

use super::*;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Colors::new(ColorLevel::TrueColor, Some((0, 0, 0))),
        Lang::En,
        Transcript::new(
            selection(),
            PermissionMode::Default,
            crate::view::test_workspace(),
        ),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn is_repaint(action: &Option<Action>) -> bool {
    matches!(action, Some(Action::Manage(Manage::Repaint)))
}

#[test]
fn the_wheel_and_the_keys_preview_the_same_way_and_esc_puts_the_theme_back() {
    let ids = schemes::ids();
    let mut app = app();
    assert_eq!(app.ink.choice.theme, ids[0], "Terminal is the default");
    app.theme_list();
    // 滚轮往下一格：焦点到第二个主题，当场换上，交给 runner 重画。
    let action = app.wheel(true);
    assert!(is_repaint(&action), "{action:?}");
    assert_eq!(app.ink.choice.theme, ids[1]);
    // 按键往下：同一条路。
    let action = app.theme_key(key(KeyCode::Down));
    assert!(is_repaint(&action), "{action:?}");
    assert_eq!(app.ink.choice.theme, ids[2]);
    // 到了顶上再往上滚：焦点不动，不重画。
    app.wheel(false);
    app.wheel(false);
    assert_eq!(app.ink.choice.theme, ids[0]);
    assert!(app.wheel(false).is_none());
    // esc：换回打开时的主题（这里已经是它，不重画），列表关上。
    app.wheel(true);
    let action = app.theme_key(key(KeyCode::Esc));
    assert!(is_repaint(&action), "{action:?}");
    assert_eq!(app.ink.choice.theme, ids[0]);
    assert!(app.overlay.is_none());
}

#[test]
fn enter_keeps_the_previewed_theme_and_asks_the_runner_to_write_it() {
    let ids = schemes::ids();
    let mut app = app();
    app.theme_list();
    app.wheel(true);
    let action = app.theme_key(key(KeyCode::Enter));
    let Some(Action::Manage(Manage::SetTheme(choice))) = action else {
        panic!("{action:?}")
    };
    assert_eq!(choice.theme, ids[1]);
    assert_eq!(app.ink.choice.theme, ids[1]);
    assert!(app.overlay.is_none());
}

#[test]
fn the_wheel_on_another_list_does_not_touch_the_theme() {
    let mut app = app();
    app.overlay = Some(Overlay::List(ListView::new(
        Purpose::Info,
        "Memory".to_owned(),
        vec![Item::default(), Item::default()],
        String::new(),
    )));
    assert!(app.wheel(true).is_none());
    assert_eq!(app.ink.choice.theme, schemes::ids()[0]);
}
