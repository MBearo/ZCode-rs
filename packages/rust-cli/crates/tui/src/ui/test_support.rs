//! 单测共用的助手（架构整理 U15：以前 `fn app(` 写了 18 份、11 种写法，`fn key(` 14 份）。
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::app::App;
use super::i18n::Lang;
use super::probe::{ColorLevel, Colors};
use crate::view::Transcript;

/// 真彩、黑底的配色。
pub(crate) fn palette() -> Colors {
    Colors::new(ColorLevel::TrueColor, Some((0, 0, 0)))
}

/// 还没有会话的界面：给定的配色、语言与新会话的权限模式；工作区是 `/Users/dev/project`。
pub(crate) fn app_with(palette: Colors, lang: Lang, mode: PermissionMode) -> App {
    App::new(
        palette,
        lang,
        Transcript::new(selection(), mode, crate::view::test_workspace()),
        Some("/Users/dev".into()),
        "0.1.0".into(),
    )
}

/// 英文、缺省权限模式的界面。
pub(crate) fn app() -> App {
    app_in(Lang::En)
}

pub(crate) fn app_in(lang: Lang) -> App {
    app_with(palette(), lang, PermissionMode::Default)
}

/// 不带修饰键的按键。
pub(crate) fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

/// ctrl 加一个字符。
pub(crate) fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}
