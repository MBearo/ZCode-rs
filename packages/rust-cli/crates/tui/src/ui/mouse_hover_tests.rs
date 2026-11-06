//! 悬停要靠移动得来（tui-select.md §9）：比较移动时记下的东西；按键、粘贴与焦点变化清掉。区域与可见行直接写进 `Hits`，
//! 当作上一帧画出来的。
use std::time::Instant;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind};

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::task_line::tests::app;

const SPOT: Rect = Rect::new(10, 5, 6, 1);

fn moved(app: &mut App, x: u16, y: u16) {
    let event = MouseEvent {
        kind: MouseEventKind::Moved,
        column: x,
        row: y,
        modifiers: KeyModifiers::NONE,
    };
    app.on_mouse(event, Instant::now());
}

fn regions(app: &mut App, regions: Vec<(Rect, Target)>) {
    app.pointer.hits.regions = regions;
}

/// transcript 从第 0 行起，第 `row` 行是可展开的块 `key` 的文字 `hello`。
fn block(app: &mut App, row: usize, key: u64) {
    let hits = &mut app.pointer.hits;
    hits.transcript = Rect::new(0, 0, 40, 10);
    hits.rows = vec![((0, 0), String::new()); 10];
    hits.rows[row] = ((key, 0), "hello".to_owned());
    hits.expandable = [key].into();
}

#[test]
fn moving_onto_a_target_lights_it() {
    let mut app = app(Lang::En);
    regions(&mut app, vec![(SPOT, Target::Shortcuts)]);
    moved(&mut app, 12, 5);
    assert_eq!(app.hovered_region(), Some(SPOT));
    moved(&mut app, 30, 5);
    assert_eq!(app.hovered_region(), None);
}

#[test]
fn a_target_drawn_under_a_still_pointer_is_not_lit_until_the_mouse_moves() {
    let mut app = app(Lang::En);
    moved(&mut app, 12, 5);
    regions(&mut app, vec![(SPOT, Target::Shortcuts)]);
    assert_eq!(app.hovered_region(), None);
    moved(&mut app, 13, 5);
    assert_eq!(app.hovered_region(), Some(SPOT));
}

#[test]
fn another_target_moved_under_a_still_pointer_is_not_lit() {
    let mut app = app(Lang::En);
    regions(&mut app, vec![(SPOT, Target::Shortcuts)]);
    moved(&mut app, 12, 5);
    regions(&mut app, vec![(SPOT, Target::OpenCockpit)]);
    assert_eq!(app.hovered_region(), None);
    // 同一个目标换了位置、仍在鼠标下面：还是它。
    regions(&mut app, vec![(Rect::new(8, 5, 10, 1), Target::Shortcuts)]);
    assert_eq!(app.hovered_region(), Some(Rect::new(8, 5, 10, 1)));
}

#[test]
fn a_key_a_paste_and_a_focus_change_clear_the_hover() {
    let mut app = app(Lang::En);
    regions(&mut app, vec![(SPOT, Target::Shortcuts)]);
    moved(&mut app, 12, 5);
    app.on_terminal_key(
        KeyEvent::new(KeyCode::Right, KeyModifiers::NONE),
        Instant::now(),
    );
    assert_eq!(app.hovered_region(), None);
    moved(&mut app, 12, 5);
    app.on_paste("draft");
    assert_eq!(app.hovered_region(), None);
    moved(&mut app, 12, 5);
    app.focus_changed();
    assert_eq!(app.hovered_region(), None);
    moved(&mut app, 12, 5);
    assert_eq!(app.hovered_region(), Some(SPOT));
}

#[test]
fn a_block_pushed_under_a_still_pointer_keeps_its_dim() {
    let mut app = app(Lang::En);
    block(&mut app, 3, 7);
    moved(&mut app, 2, 3);
    assert_eq!(app.hovered_block(), Some(7));
    // 流式输出把另一个块推到鼠标下面。
    block(&mut app, 3, 9);
    assert_eq!(app.hovered_block(), None);
    moved(&mut app, 3, 3);
    assert_eq!(app.hovered_block(), Some(9));
}
