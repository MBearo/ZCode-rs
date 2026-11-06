//! 页的一屏（`page_frame`，tui-kit.md §4.0「页」）：正文换成页、底部栈留着；看不见的行写在第一与最后一行；页的键在槽里。
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::testing::selection;
use rust_cli_tui_kit::render::{Span, StyledLine};

use super::*;
use crate::ui::i18n::Lang;
use crate::ui::page_frame::PageBody;
use crate::ui::probe::{ColorLevel, Colors};
use crate::view::Transcript;

fn app() -> App {
    App::new(
        Colors::new(ColorLevel::TrueColor, Some((21, 22, 26)))
            .with_foreground(Some((214, 216, 220))),
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

fn texts(lines: &[StyledLine]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.plain().trim_end().to_owned())
        .collect()
}

#[test]
fn a_page_replaces_the_body_and_keeps_the_bottom_stack() {
    let mut app = app();
    let page = PageBody {
        lines: (0..40)
            .map(|i| StyledLine::new(vec![Span::plain(format!("  line {i}"))]))
            .collect(),
        top: 5,
        slot: Some(vec![Span::plain("esc back")]),
        owns_keys: true,
        fit: false,
    };
    let now = app.started;
    let screen = page_frame(&mut app, &page, now, 0, 60, 20);
    let rows = texts(&screen);
    assert_eq!(rows.len(), 20);
    // 顶上一行说上面还有几行，底部栈上面那一行说下面还有几行。
    assert_eq!(rows[0], "   ↑ 6 more");
    let board = rows.iter().position(|r| r.starts_with('▄')).unwrap();
    assert!(rows[board - 1].starts_with("   ↓ "), "{rows:#?}");
    assert_eq!(rows[1], "  line 6");
    // 板与状态栏留着；槽是页的键，拿键时没有占位句。
    assert!(rows[board + 1].ends_with("esc back"), "{rows:#?}");
    assert!(rows[board + 2].starts_with('▀'), "{rows:#?}");
    assert!(rows[19].contains("~/project"), "{rows:#?}");
}
