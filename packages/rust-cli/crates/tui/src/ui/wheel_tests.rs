use std::collections::BTreeMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;

use super::{WHEEL_LINES, WheelLines};
use crate::ui::app::App;
use crate::ui::i18n::Lang;
use crate::ui::mouse::{Grain, Point, Selection};
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Colors::new(ColorLevel::TrueColor, Some((21, 22, 26))),
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

/// 正文顶行：跟随时停在末尾，锚定时离末尾 `scroll` 行（同 `draw.rs`）。
fn top(app: &App) -> usize {
    app.transcript_lines
        .saturating_sub(app.port.scroll)
        .saturating_sub(app.viewport)
}

fn vars(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

/// 每格 3 行，同 Puffin；终端是什么都一样（以前 VS Code、Windows Terminal、Cursor、JetBrains 各有加速）。
#[test]
fn every_notch_scrolls_three_lines() {
    assert_eq!(WHEEL_LINES, 3);
    for term in [
        vec![],
        vec![("TERM_PROGRAM", "vscode")],
        vec![("WT_SESSION", "x")],
        vec![("CURSOR_TRACE_ID", "x")],
        vec![("TERMINAL_EMULATOR", "JetBrains-JediTerm")],
    ] {
        assert_eq!(WheelLines::new(&vars(&term)).rows(), 3, "{term:?}");
    }
    assert_eq!(WheelLines::default().rows(), 3);
}

/// `ZCODE_SCROLL_SPEED` 代替这个 3：四舍五入，1 到 20；不是正数时不管它。
#[test]
fn the_scroll_speed_variable_replaces_the_three() {
    let lines = |v: &str| WheelLines::new(&vars(&[("ZCODE_SCROLL_SPEED", v)])).rows();
    assert_eq!(lines("5"), 5);
    assert_eq!(lines("1.6"), 2);
    assert_eq!(lines("0.2"), 1);
    assert_eq!(lines("50"), 20);
    assert_eq!(lines("0"), 3);
    assert_eq!(lines("fast"), 3);
}

/// 内容放得下时往上滚也锚定，顶行停在 0；之后长出的内容在下面（Puffin `scroll_to`：目标行在跟随的顶行上面）。往下滚
/// 与 ctrl+Home（目标行 0 不在跟随的顶行上面）跟随。修复前偏移夹到 0，往上滚丢了，视图跟到底（孪生 scroll-separator）。
#[test]
fn a_wheel_up_holds_the_top_even_when_it_all_fits() {
    let mut app = app();
    app.viewport = 30;
    app.content_resized(12);
    app.wheel(false);
    assert!(app.port.scroll > 0, "锚定");
    assert_eq!(top(&app), 0);
    for lines in [20, 31, 57] {
        app.content_resized(lines);
        assert_eq!(top(&app), 0, "{lines} 行时顶行还是 0");
    }
    app.wheel(true);
    assert_eq!(top(&app), 3);
    app.shown_top = 3;
    for _ in 0..10 {
        app.wheel(true);
        app.shown_top = top(&app);
    }
    assert_eq!(app.port.scroll, 0, "滚到跟随的顶行就跟随");

    let mut short = self::app();
    short.viewport = 30;
    short.content_resized(12);
    short.scroll_home();
    assert_eq!(short.port.scroll, 0, "放得下时 ctrl+Home 不锚定");
    short.wheel(false);
    short.wheel(true);
    assert_eq!(short.port.scroll, 0, "放得下时往下滚回到跟随");
}

/// ctrl+n 与 ctrl+End 回到末尾（tui-kit.md §4.0）：草稿、光标与对话区的选区都不动。
#[test]
fn ctrl_n_goes_back_to_the_end_and_leaves_the_draft_alone() {
    for code in [KeyCode::Char('n'), KeyCode::End] {
        let mut app = app();
        app.composer.area.insert('h');
        app.composer.area.insert('i');
        app.composer.area.left();
        let cursor = app.composer.area.cursor();
        let point = Point {
            key: 0,
            offset: 0,
            col: 0,
        };
        let selection = Selection {
            anchor: point,
            head: point,
            grain: Grain::Cell,
            dragging: false,
            edge: 0,
            copied: None,
        };
        app.pointer.selection = Some(selection);
        app.port.scroll = 7;
        app.port.new_activity = true;
        let now = app.started;
        assert!(
            app.on_key(KeyEvent::new(code, KeyModifiers::CONTROL), now)
                .is_none()
        );
        assert_eq!(app.port.scroll, 0, "{code:?}");
        assert!(!app.port.new_activity, "{code:?}");
        assert_eq!(app.composer.area.text(), "hi", "{code:?}");
        assert_eq!(app.composer.area.cursor(), cursor, "{code:?}");
        assert!(app.pointer.selection.is_some(), "{code:?}");
    }
}
