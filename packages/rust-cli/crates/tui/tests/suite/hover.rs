//! 悬停要靠移动得来（docs/specs/rust-cli/tui-select.md §9）：鼠标不动时被画到它下面的可点击处不高亮，动一下才高亮；按键与
//! 焦点变化清掉悬停。
use crossterm::event::{KeyCode, MouseEventKind};
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui_kit::render::attr;

use super::support::{GatedModel, Setup, Shot, Ui, finish, row_of, with_ui};

const HINT: &str = "← agent cockpit";

/// 槽里驾驶舱的提示所在的（列，行）。
fn hint_at(screen: &str) -> (u16, u16) {
    let row = row_of(screen, HINT).unwrap();
    let line = screen.lines().nth(usize::from(row)).unwrap();
    let at = line.find(HINT).unwrap();
    (u16::try_from(line[..at].chars().count()).unwrap(), row)
}

/// 提示的第 3 格带着悬停：底色与提示前面两格的空白（输入框的板）不同，或没有底色的主题里的下划线（`mouse::paint_hover`）。
fn lit(shot: &Shot, (x, y): (u16, u16)) -> bool {
    let cell = &shot[(x + 2, y)];
    cell.bg != shot[(x - 2, y)].bg || cell.has(attr::UNDERLINE)
}

/// 回合里鼠标停在 `esc stop` 上（不可点击）；回合结束时 `esc stop` 走了，提示左移到鼠标下面。以前存的只是位置，提示被
/// 高亮；现在不高亮，动一下鼠标才高亮，按键之后又不高亮。
#[tokio::test(flavor = "multi_thread")]
async fn a_hint_moved_under_a_still_pointer_is_not_lit_until_the_mouse_moves() {
    let (model, mut senders) = GatedModel::new(1);
    let setup = Setup {
        width: 100,
        height: 40,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        let idle = ui.wait_for(HINT).await;
        let (x, y) = hint_at(&idle);
        ui.submit("hello");
        let busy = ui.wait_for("esc stop").await;
        // 回合里提示在右边：先移到它上面、看到高亮，再移到 `esc stop` 上（不可点击）、看到高亮没了——这样知道这次移动在
        // 回合结束之前已经处理完。
        let during = hint_at(&busy);
        ui.mouse(MouseEventKind::Moved, during.0 + 2, during.1);
        wait_lit(&ui, during, true).await;
        ui.mouse(MouseEventKind::Moved, x + 2, y);
        wait_lit(&ui, during, false).await;
        finish(senders.remove(0), text_response(&["hi there"]));
        let done = ui
            .wait("the turn to end", |s| {
                s.contains("hi there") && !s.contains("esc stop")
            })
            .await;
        assert_eq!(hint_at(&done), (x, y), "{done}");
        assert!(!lit(&ui.frame(), (x, y)), "{}", ui.frame().text());
        ui.mouse(MouseEventKind::Moved, x + 3, y);
        wait_lit(&ui, (x, y), true).await;
        ui.key(KeyCode::Right);
        wait_lit(&ui, (x, y), false).await;
    })
    .await;
}

/// 焦点变化清掉悬停（离开窗口后终端不再报告鼠标）；回来后动一下鼠标才又有。粘贴的草稿会换掉槽里的提示，粘贴在
/// `ui/mouse_hover_tests.rs` 里验。
#[tokio::test(flavor = "multi_thread")]
async fn a_focus_change_clears_the_hover() {
    let (model, _senders) = GatedModel::new(0);
    let setup = Setup {
        width: 100,
        height: 40,
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        let at = hint_at(&ui.wait_for(HINT).await);
        ui.mouse(MouseEventKind::Moved, at.0 + 2, at.1);
        wait_lit(&ui, at, true).await;
        ui.focus(false);
        wait_lit(&ui, at, false).await;
        ui.focus(true);
        ui.mouse(MouseEventKind::Moved, at.0 + 3, at.1);
        wait_lit(&ui, at, true).await;
    })
    .await;
}

async fn wait_lit(ui: &Ui, at: (u16, u16), want: bool) {
    super::resume_page::wait_frame(ui, if want { "the hover" } else { "no hover" }, |shot| {
        lit(shot, at) == want
    })
    .await;
}
