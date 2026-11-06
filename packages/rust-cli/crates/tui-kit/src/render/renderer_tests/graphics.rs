//! 图形（画在文字之后的图片，docs/puffin-tui.md §4.4）与停在光标格的隐藏光标（输入法跟着它）。
//! 移植自 packages/tui/tui/test/src/zcode/tui/render/RendererSuite.scala（feat/tui a2bf878b34）。
use super::{Rig, lines, sl};
use crate::render::renderer::Graphic;
use crate::render::{Span, Style, StyledLine, ansi};

// 模拟器里图形的 `draw` 是普通文字：落在图片会落的地方，也像 sixel / iTerm2 的图片那样被盖在它上面的文字擦掉。
fn pic(row: usize, col: usize) -> Graphic {
    pic_with(row, col, "")
}

fn pic_with(row: usize, col: usize, erase: &str) -> Graphic {
    Graphic {
        id: 1,
        row,
        col,
        rows: 1,
        cols: 2,
        draw: "@@".to_string(),
        erase: erase.to_string(),
    }
}

#[test]
fn a_graphic_is_drawn_after_the_text_at_its_cell() {
    let mut t = Rig::new(20, 3);
    t.paint(20, 3, &lines(&["", "  text", ""]), &[pic(1, 0)], None);
    assert_eq!(t.emu.line(1), "@@text");
    assert!(
        t.last.find("@@") > t.last.find("text"),
        "drawn after the text"
    );
}

#[test]
fn an_unchanged_graphic_is_not_drawn_again_even_when_text_elsewhere_changes() {
    let mut t = Rig::new(20, 3);
    t.paint(20, 3, &lines(&["a", "", ""]), &[pic(1, 0)], None);
    let before = t.writes;
    t.paint(20, 3, &lines(&["a", "", ""]), &[pic(1, 0)], None);
    assert_eq!(t.writes, before, "same text and graphics: no write");
    t.paint(20, 3, &lines(&["b", "", ""]), &[pic(1, 0)], None);
    assert!(!t.last.contains("@@"), "{:?}", t.last);
}

#[test]
fn a_moved_graphic_its_erase_is_sent_its_old_cells_are_rewritten_and_it_is_drawn_at_the_new_cell() {
    // erase 代替 kitty 的删除：屏幕不显示的一个序列。
    let del = "\x1b[5z";
    let mut t = Rig::new(20, 3);
    t.paint(20, 3, &lines(&["", "", ""]), &[pic_with(0, 0, del)], None);
    assert_eq!(t.emu.line(0), "@@");
    t.paint(20, 3, &lines(&["", "", ""]), &[pic_with(2, 3, del)], None);
    assert!(t.last.contains(del), "{:?}", t.last);
    assert_eq!(t.emu.line(0), "", "the old cells were rewritten");
    assert_eq!(t.emu.line(2), "   @@");
}

#[test]
fn a_removed_graphic_is_erased_and_its_cells_rewritten() {
    let mut t = Rig::new(20, 3);
    t.paint(20, 3, &lines(&["", "", ""]), &[pic(1, 0)], None);
    t.render(20, 3, &lines(&["", "", ""]));
    assert_eq!(t.emu.line(1), "");
}

#[test]
fn a_full_repaint_draws_the_graphics_again() {
    let mut t = Rig::new(20, 3);
    t.paint(20, 3, &lines(&["", "", ""]), &[pic(1, 0)], None);
    t.r.invalidate();
    t.paint(20, 3, &lines(&["", "", ""]), &[pic(1, 0)], None);
    assert!(t.last.contains("@@"), "{:?}", t.last);
    assert_eq!(t.emu.line(1), "@@");
}

#[test]
fn a_rewritten_risky_row_draws_the_graphic_on_it_again() {
    // 宽度有争议的行整行重写（带擦到行尾），也就擦掉了这一行上的 sixel / iTerm2 图片。
    let mut t = Rig::new(20, 3);
    t.paint(20, 3, &lines(&["", "    ✅a", ""]), &[pic(1, 0)], None);
    t.paint(20, 3, &lines(&["", "    ✅b", ""]), &[pic(1, 0)], None);
    assert!(t.emu.line(1).starts_with("@@"), "{}", t.emu.line(1));
}

/* ---- 隐藏的光标停在光标格（输入法跟着它；docs/puffin-tui.md §4.4） ---- */

/// 一行里的 `before`、一个光标格 `caret`、`after`。
fn caret_line(before: &str, caret: &str, after: &str) -> StyledLine {
    StyledLine::new(vec![
        Span::new(before.to_string(), Style::DEFAULT),
        Span::new(caret.to_string(), Style::CARET),
        Span::new(after.to_string(), Style::DEFAULT),
    ])
}

fn caret_at(before: &str) -> StyledLine {
    caret_line(before, " ", "")
}

#[test]
fn a_frame_ends_with_the_hidden_cursor_on_the_caret_cell() {
    let mut t = Rig::new(20, 4);
    t.render(
        20,
        4,
        &[
            sl("answer streaming"),
            sl(""),
            caret_at(" › ab"),
            sl("status"),
        ],
    );
    assert_eq!((t.emu.row, t.emu.col), (2, 5));
    let tail = format!("{}3;6H{}{}", ansi::CSI, ansi::RESET, ansi::SYNC_END);
    assert!(t.last.ends_with(&tail), "{:?}", t.last);
}

#[test]
fn a_patch_elsewhere_still_leaves_the_cursor_on_the_caret_not_on_the_last_cell_written() {
    let mut t = Rig::new(20, 4);
    t.render(
        20,
        4,
        &[sl("answer"), sl(""), caret_at(" › ab"), sl("status")],
    );
    // 流式的回答在第 0 行变长，然后状态行变化：每次它都是最后写的格子。
    t.render(
        20,
        4,
        &[sl("answer grows"), sl(""), caret_at(" › ab"), sl("status")],
    );
    assert_eq!((t.emu.row, t.emu.col), (2, 5));
    t.render(
        20,
        4,
        &[
            sl("answer grows"),
            sl(""),
            caret_at(" › ab"),
            sl("status 2"),
        ],
    );
    assert_eq!((t.emu.row, t.emu.col), (2, 5));
}

#[test]
fn a_moved_caret_moves_the_cursor() {
    let mut t = Rig::new(20, 3);
    t.render(20, 3, &[sl(""), caret_at(" › ab"), sl("")]);
    t.render(20, 3, &[sl(""), caret_line(" › a", "b", ""), sl("")]);
    assert_eq!((t.emu.row, t.emu.col), (1, 4));
}

#[test]
fn the_cursor_is_parked_after_the_graphics_are_drawn() {
    let mut t = Rig::new(20, 3);
    t.paint(
        20,
        3,
        &[sl(""), caret_at(" › "), sl("")],
        &[pic(0, 10)],
        None,
    );
    assert_eq!(t.emu.line(0), format!("{}@@", " ".repeat(10)));
    assert_eq!((t.emu.row, t.emu.col), (1, 3));
    let park = format!("{}2;4H", ansi::CSI);
    assert!(t.last.rfind(&park) > t.last.find("@@"), "{:?}", t.last);
}

#[test]
fn a_caret_on_a_wide_glyph_parks_on_its_first_column() {
    let mut t = Rig::new(20, 2);
    t.render(20, 2, &[caret_line(" › 中", "文", "字"), sl("")]);
    assert_eq!((t.emu.row, t.emu.col), (0, 5));
}

#[test]
fn the_first_caret_cell_wins() {
    let mut t = Rig::new(20, 3);
    t.render(20, 3, &[sl(""), caret_at(" a"), caret_at("   b")]);
    assert_eq!((t.emu.row, t.emu.col), (1, 2));
}

#[test]
fn without_a_caret_the_cursor_is_not_moved_after_the_last_write() {
    let mut t = Rig::new(20, 3);
    t.render(20, 3, &lines(&["a", "", ""]));
    t.render(20, 3, &lines(&["a", "bc", ""]));
    assert_eq!((t.emu.row, t.emu.col), (1, 2));
    let tail = format!("bc{}{}", ansi::RESET, ansi::SYNC_END);
    assert!(t.last.ends_with(&tail), "{:?}", t.last);
}

#[test]
fn an_identical_frame_with_a_caret_writes_nothing() {
    let mut t = Rig::new(20, 3);
    t.render(20, 3, &[sl("x"), caret_at(" › "), sl("")]);
    let before = t.writes;
    t.render(20, 3, &[sl("x"), caret_at(" › "), sl("")]);
    assert_eq!(t.writes, before);
}

#[test]
fn a_resize_repaints_and_parks_the_cursor_on_the_caret() {
    let mut t = Rig::new(20, 3);
    t.render(20, 3, &[sl("x"), caret_at(" › "), sl("")]);
    t.render(16, 4, &[sl("x"), sl(""), caret_at(" › hi"), sl("")]);
    assert_eq!((t.emu.row, t.emu.col), (2, 5));
}
