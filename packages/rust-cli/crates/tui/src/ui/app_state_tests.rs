//! `App` 的子状态的重置（架构整理 U3）。

use crate::ui::app::App;
use crate::ui::mouse::{Copied, Grain, Point, ScreenSelection, Selection};
use crate::ui::test_support::app;

fn read_somewhere(app: &mut App) {
    let point = Point {
        key: 1,
        offset: 0,
        col: 0,
    };
    app.port.scroll = 7;
    app.port.new_activity = true;
    app.port.expanded_cells.insert(3);
    app.port.restore_bottom = Some((1, 0));
    app.pointer.selection = Some(Selection {
        anchor: point,
        head: point,
        grain: Grain::Cell,
        dragging: false,
        edge: 0,
        copied: Some(Copied::of("x")),
    });
    app.pointer.screen_selection = Some(ScreenSelection {
        anchor: (0, 1),
        head: (5, 1),
        dragging: false,
        copied: true,
    });
}

/// 换显示的 transcript（开始、换一个、结束查看子代理）：读位置回到初始值，transcript 与全屏视图的选区都清掉。修前这份
/// 重置在 runner 里另写一份，漏了全屏视图的选区（详细视图打开时异步换了 transcript，选区留在换了内容的画面上）。
#[test]
fn changing_the_shown_transcript_resets_the_reading_position_and_both_selections() {
    let mut app = app();
    read_somewhere(&mut app);
    app.shown_changed();
    assert_eq!(app.port.scroll, 0);
    assert!(!app.port.new_activity);
    assert!(app.port.expanded_cells.is_empty());
    assert_eq!(app.port.restore_bottom, None);
    assert_eq!(app.pointer.selection, None);
    assert_eq!(app.pointer.screen_selection, None);
}

/// 离开会话用同一份重置，另外清空会话的快照；请求统计的序号照旧往前走（在途的回执作废）。
#[test]
fn leaving_the_session_also_clears_its_snapshots() {
    let mut app = app();
    read_somewhere(&mut app);
    let in_flight = app.snapshots.request_stats.begin(0);
    app.reset();
    assert_eq!(app.port.scroll, 0);
    assert_eq!(app.pointer.screen_selection, None);
    assert!(app.snapshots.background.is_empty());
    assert!(app.snapshots.agents.entries.is_empty());
    assert!(
        !app.snapshots.request_stats.arrived(in_flight, None),
        "the reply sent before leaving is dropped"
    );
}
