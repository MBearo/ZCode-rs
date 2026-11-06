//! 跟着草稿的一层（draft_layer.rs）：打字进草稿并过滤、草稿不再以命令开头时关上、esc 留着草稿、`?` 按键表。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use crate::ui::app::{Action, App, Overlay};
use crate::ui::i18n::Lang;
use crate::ui::list::{Choice, Item, ListView, Purpose};
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

fn type_text(app: &mut App, text: &str) {
    for c in text.chars() {
        app.on_key(key(KeyCode::Char(c)), Instant::now());
    }
}

fn models() -> ListView {
    let item = |label: &str| Item {
        label: label.to_owned(),
        value: Choice::Model(ModelSelection::new("p", label)),
        ..Item::default()
    };
    ListView::new(
        Purpose::Model,
        "Select model".into(),
        vec![item("glm-5"), item("kimi-k2"), item("glm-4.6")],
        "No matches".into(),
    )
    .filterable()
}

fn list(app: &App) -> Option<&ListView> {
    match &app.overlay {
        Some(Overlay::List(list)) => Some(list),
        _ => None,
    }
}

#[test]
fn a_list_follows_the_draft() {
    let mut app = app();
    app.show_list(models());
    assert_eq!(app.composer.area.text(), "/model ");
    type_text(&mut app, "kimi");
    assert_eq!(app.composer.area.text(), "/model kimi");
    let shown = list(&app).expect("still open");
    assert_eq!(shown.filter.as_deref(), Some("kimi"));
    assert_eq!(shown.visible().len(), 1);
    // 删到命令不完整时关上。
    for _ in 0..6 {
        app.on_key(key(KeyCode::Backspace), Instant::now());
    }
    assert_eq!(app.composer.area.text(), "/mode");
    assert!(app.overlay.is_none());
}

#[test]
fn esc_closes_a_list_and_keeps_the_draft() {
    let mut app = app();
    app.show_list(models());
    type_text(&mut app, "glm");
    app.on_key(key(KeyCode::Esc), Instant::now());
    assert!(app.overlay.is_none());
    assert_eq!(app.composer.area.text(), "/model glm");
}

#[test]
fn a_late_list_does_not_overwrite_typing() {
    let mut app = app();
    type_text(&mut app, "hello");
    app.show_list(models());
    assert_eq!(app.composer.area.text(), "hello");
}

#[test]
fn the_key_sheet_is_the_typed_question_mark() {
    let mut app = app();
    type_text(&mut app, "?");
    assert_eq!(app.composer.area.text(), "?");
    assert!(matches!(app.overlay, Some(Overlay::Shortcuts)));
    // esc 关上、`?` 留着。
    app.on_key(key(KeyCode::Esc), Instant::now());
    assert!(app.overlay.is_none());
    assert_eq!(app.composer.area.text(), "?");
    // 打下去：草稿不再是 `?`。
    type_text(&mut app, "x");
    assert_eq!(app.composer.area.text(), "?x");
    assert!(app.overlay.is_none());
    // 删回 `?`：又打开。
    app.on_key(key(KeyCode::Backspace), Instant::now());
    assert!(matches!(app.overlay, Some(Overlay::Shortcuts)));
    // ⏎ 关上并清空草稿，什么都不发。
    assert!(app.on_key(key(KeyCode::Enter), Instant::now()).is_none());
    assert!(app.overlay.is_none());
    assert!(app.composer.area.text().is_empty());
}

/// 打出命令后面的空格就打开列表（tui-kit.md §4.0「板」，用户报告 2026-10-31：以前只有 ⏎ 才打开）。
#[test]
fn a_typed_space_opens_the_theme_list() {
    let mut app = app();
    type_text(&mut app, "/theme");
    assert!(app.overlay.is_none());
    type_text(&mut app, " ");
    assert_eq!(app.composer.area.text(), "/theme ");
    assert_eq!(list(&app).expect("open").purpose, Purpose::Theme);
}

#[test]
fn a_typed_space_asks_for_the_model_list_once() {
    let mut app = app();
    type_text(&mut app, "/mode");
    type_text(&mut app, "l");
    let action = app.on_key(key(KeyCode::Char(' ')), Instant::now());
    assert!(matches!(action, Some(Action::OpenModels)), "{action:?}");
    // 目录还没到：接着打字、⏎ 不再读一次，草稿留着。
    type_text(&mut app, "kimi");
    let again = app.on_key(key(KeyCode::Enter), Instant::now());
    assert!(again.is_none(), "{again:?}");
    assert_eq!(app.composer.area.text(), "/model kimi");
    // 目录到了：当场按草稿过滤。
    app.open_models(&[]);
    assert!(!app.models_opening);
    app.show_list(models());
    let shown = list(&app).expect("open");
    assert_eq!(shown.filter.as_deref(), Some("kimi"));
    assert_eq!(shown.visible().len(), 1);
}

/// 只有打进末尾的空格才打开：`/mode ` 不是（它的列表不过滤，§4.0），别名 `/effort ` 不是（后面的词不是模型名），光标不在末尾时
/// 也不是。
#[test]
fn other_spaces_only_edit_the_draft() {
    let mut app = app();
    type_text(&mut app, "/mode ");
    assert!(app.overlay.is_none());
    assert_eq!(app.composer.area.text(), "/mode ");

    let mut app = self::app();
    type_text(&mut app, "/effort");
    let action = app.on_key(key(KeyCode::Char(' ')), Instant::now());
    assert!(action.is_none(), "{action:?}");
    assert_eq!(app.composer.area.text(), "/effort ");

    let mut app = self::app();
    type_text(&mut app, "/theme");
    app.on_key(key(KeyCode::Left), Instant::now());
    type_text(&mut app, " ");
    assert!(app.overlay.is_none());
    assert_eq!(app.composer.area.text(), "/them e");
}

/// 命令架上 ⇥：`/theme` 打开列表，别的命令照旧只补全。
#[test]
fn tab_on_the_shelf_opens_the_list() {
    let mut app = app();
    type_text(&mut app, "/them");
    app.on_key(key(KeyCode::Tab), Instant::now());
    assert_eq!(app.composer.area.text(), "/theme ");
    assert_eq!(list(&app).expect("open").purpose, Purpose::Theme);

    let mut app = self::app();
    type_text(&mut app, "/renam");
    app.on_key(key(KeyCode::Tab), Instant::now());
    assert_eq!(app.composer.area.text(), "/rename ");
    assert!(app.overlay.is_none());
}

/// 别名 `/effort` 带什么词都只打开模型选择器；`/theme 词` ⏎ 按词过滤，不直接换。
#[test]
fn words_only_open_the_list() {
    let mut app = app();
    app.composer.area.set_text("/effort low");
    let action = app.submit();
    assert!(matches!(action, Some(Action::OpenModels)), "{action:?}");
    assert_eq!(app.composer.area.text(), "/model ");

    let mut app = self::app();
    let was = app.ink.choice.theme;
    app.composer.area.set_text("/theme nord");
    assert!(app.submit().is_none());
    assert_eq!(app.composer.area.text(), "/theme nord");
    let shown = list(&app).expect("open");
    assert_eq!(shown.purpose, Purpose::Theme);
    assert_eq!(shown.filter.as_deref(), Some("nord"));
    assert_eq!(app.ink.choice.theme, was);
}
