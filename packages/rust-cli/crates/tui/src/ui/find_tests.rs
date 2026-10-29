//! 搜索模式的状态与按键（docs/specs/rust-cli/tui-input.md §2.3.1）：进入的条件、查询的编辑、↑ ↓ 的累计、enter / esc /
//! 空查询 backspace 的退出、粘贴、查看子代理时的 `/`。画面的断言在 `find_view_tests.rs`。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::*;
use crate::ui::composer::InputMode;
use crate::ui::palette::{ColorLevel, Palette};
use crate::view::Transcript;

pub(super) fn app() -> App {
    App::new(
        Palette::new(ColorLevel::TrueColor, Some((0, 0, 0))),
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

pub(super) fn key(app: &mut App, code: KeyCode) -> Option<Action> {
    app.on_key(KeyEvent::new(code, KeyModifiers::NONE), Instant::now())
}

pub(super) fn ctrl(app: &mut App, c: char) -> Option<Action> {
    app.on_key(
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL),
        Instant::now(),
    )
}

pub(super) fn typed(app: &mut App, text: &str) {
    for c in text.chars() {
        key(app, KeyCode::Char(c));
    }
}

fn query(app: &App) -> Option<&str> {
    app.find.as_ref().map(|find| find.query.as_str())
}

#[test]
fn typing_two_slashes_enters_search_mode_and_clears_the_input() {
    let mut app = app();
    key(&mut app, KeyCode::Char('/'));
    assert!(app.composer.popup.is_some());
    key(&mut app, KeyCode::Char('/'));
    assert_eq!(query(&app), Some(""));
    assert!(app.composer.is_empty());
    assert!(app.composer.popup.is_none());
    // 清空的 `/` 不记进历史：↑ 翻不到它（搜索模式里 ↑ 本来就不翻历史，退出后再看）。
    key(&mut app, KeyCode::Esc);
    assert!(app.find.is_none());
    key(&mut app, KeyCode::Up);
    assert!(app.composer.is_empty());
}

#[test]
fn a_slash_elsewhere_is_just_text() {
    // `/c` 之后、`a/` 之后、光标不在末尾、bash 模式里都不切。
    let mut app = app();
    typed(&mut app, "/c/");
    assert!(app.find.is_none());
    assert_eq!(app.composer.area.text(), "/c/");
    let mut app = app_with("a");
    typed(&mut app, "//");
    assert!(app.find.is_none());
    assert_eq!(app.composer.area.text(), "a//");
    let mut app = fresh();
    typed(&mut app, "/");
    key(&mut app, KeyCode::Left);
    typed(&mut app, "/");
    assert!(app.find.is_none());
    assert_eq!(app.composer.area.text(), "//");
    let mut app = fresh();
    typed(&mut app, "!//");
    assert_eq!(app.composer.mode, InputMode::Bash);
    assert!(app.find.is_none());
    assert_eq!(app.composer.area.text(), "//");
    // alt+/ 不是手打的文字。
    let mut app = fresh();
    typed(&mut app, "/");
    app.on_key(
        KeyEvent::new(KeyCode::Char('/'), KeyModifiers::ALT),
        Instant::now(),
    );
    assert!(app.find.is_none());
}

fn fresh() -> App {
    app()
}

fn app_with(text: &str) -> App {
    let mut app = app();
    typed(&mut app, text);
    app
}

#[test]
fn a_pasted_double_slash_stays_in_the_input() {
    let mut app = app();
    app.on_paste("//");
    assert!(app.find.is_none());
    assert_eq!(app.composer.area.text(), "//");
    // 粘贴的 `/` 之后手打 `/`：手打的那一个切模式。
    let mut app = fresh();
    app.on_paste("/");
    key(&mut app, KeyCode::Char('/'));
    assert_eq!(query(&app), Some(""));
}

#[test]
fn the_query_takes_typing_backspace_and_paste() {
    let mut app = app_with("//");
    typed(&mut app, "ab搜e\u{301}");
    assert_eq!(query(&app), Some("ab搜e\u{301}"));
    // backspace 按字素删：`e` 加组合符一次删掉。
    key(&mut app, KeyCode::Backspace);
    assert_eq!(query(&app), Some("ab搜"));
    key(&mut app, KeyCode::Backspace);
    assert_eq!(query(&app), Some("ab"));
    app.on_paste("c\nd\r\ne");
    assert_eq!(query(&app), Some("abc d e"));
    assert!(app.find.as_ref().unwrap().pick.nearest);
    // 输入框不收这些键：草稿还是空的。
    assert!(app.composer.is_empty());
    // 别的键不生效：`?` 是查询里的字，shift+tab、ctrl+r、ctrl+t 什么都不做。
    typed(&mut app, "?");
    assert_eq!(query(&app), Some("abc d e?"));
    app.on_key(
        KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT),
        Instant::now(),
    );
    ctrl(&mut app, 'r');
    ctrl(&mut app, 't');
    assert!(app.overlay.is_none() && app.composer.search.is_none() && !app.tasks.expanded);
    assert_eq!(query(&app), Some("abc d e?"));
}

#[test]
fn arrows_and_ctrl_p_n_step_only_when_there_are_matches() {
    let mut app = app_with("//x");
    key(&mut app, KeyCode::Up);
    assert_eq!(app.find.as_ref().unwrap().pick.step, 0);
    app.find.as_mut().unwrap().count = 3;
    key(&mut app, KeyCode::Up);
    key(&mut app, KeyCode::Up);
    ctrl(&mut app, 'n');
    assert_eq!(app.find.as_ref().unwrap().pick.step, -1);
    key(&mut app, KeyCode::Down);
    ctrl(&mut app, 'p');
    ctrl(&mut app, 'p');
    assert_eq!(app.find.as_ref().unwrap().pick.step, -2);
    // ↑ ↓ 不翻输入历史。
    assert!(app.composer.is_empty());
}

#[test]
fn esc_ctrl_c_and_backspace_on_an_empty_query_leave_without_interrupting() {
    for leave in [KeyCode::Esc, KeyCode::Backspace] {
        let mut app = app_with("//");
        app.scroll = 0;
        assert_eq!(key(&mut app, leave), None);
        assert!(app.find.is_none());
    }
    let mut app = app_with("//abc");
    assert_eq!(ctrl(&mut app, 'c'), None);
    assert!(app.find.is_none());
    // 非空查询的 backspace 只删字。
    let mut app = app_with("//a");
    key(&mut app, KeyCode::Backspace);
    assert_eq!(query(&app), Some(""));
    key(&mut app, KeyCode::Backspace);
    assert!(app.find.is_none());
    // ctrl+o：退出，打开详细视图。
    let mut app = app_with("//a");
    ctrl(&mut app, 'o');
    assert!(app.find.is_none());
    assert!(matches!(
        app.overlay,
        Some(crate::ui::app::Overlay::Detail(_))
    ));
}

#[test]
fn enter_stops_on_the_match_and_keeps_a_peeked_block_open() {
    let mut app = app_with("//a");
    app.find.as_mut().unwrap().current = Some(Hit {
        key: 7,
        ordinal: 0,
        line: 2,
        col: 0,
        peeked: true,
    });
    app.scroll = 12;
    assert_eq!(key(&mut app, KeyCode::Enter), None);
    assert!(app.find.is_none());
    assert!(app.expanded_cells.contains(&7));
    assert_eq!(app.scroll, 12);
    assert!(app.restore_bottom.is_none());
    // 没有当前匹配：同 esc（回到底部）。
    let mut app = app_with("//zzz");
    app.scroll = 12;
    key(&mut app, KeyCode::Enter);
    assert!(app.find.is_none());
    assert_eq!(app.scroll, 0);
}

#[test]
fn esc_returns_to_where_the_search_started() {
    // 进入时上滚过：记下视口最后一行的归属，esc 之后下一帧放回去。
    let mut app = app();
    app.scroll = 5;
    app.hits.rows = vec![((3, 0), "a".into()), ((3, 1), "b".into())];
    typed(&mut app, "//q");
    assert_eq!(app.find.as_ref().unwrap().origin, Some((3, 1)));
    app.scroll = 40;
    key(&mut app, KeyCode::Esc);
    assert_eq!(app.restore_bottom, Some((3, 1)));
    // 进入时跟随末尾：esc 马上回到底部。
    let mut app = app_with("//q");
    app.scroll = 40;
    app.new_activity = true;
    key(&mut app, KeyCode::Esc);
    assert_eq!((app.scroll, app.new_activity), (0, false));
}

#[test]
fn clearing_the_session_ends_the_search() {
    let mut app = app_with("//q");
    app.restore_bottom = Some((1, 0));
    app.reset();
    assert!(app.find.is_none() && app.restore_bottom.is_none());
}
