//! 屏幕自洽检查背后的解码器读回渲染器的字节（docs/puffin-tui.md §7「渲染基准（L0）」）：读回的就是这一帧自己的格子网，
//! 整屏画的与 diff 的都是。移植自 packages/tui/bench/test/src/zcode/tui/bench/VtSuite.scala 里用渲染器的测试
//! （feat/tui a2bf878b34 加 bench 的改动）；不用渲染器的在 `testing/vt_tests.rs`。
use crate::render::renderer::{Graphic, Renderer};
use crate::render::{Color, Decor, Span, Style, StylePool, StyledLine, WidthClass};
use crate::testing::vt::{Vt, VtCell, expected};
use crate::testing::width_lock::widths;

const W: usize = 40;
const H: usize = 6;
const BRAND: Color = Color::TrueColor(72, 160, 255);

fn sp(text: impl Into<String>) -> Span {
    Span::new(text.into(), Style::DEFAULT)
}

fn sps(text: &str, style: Style) -> Span {
    Span::new(text.to_string(), style)
}

/// 一屏上有样式、颜色、一个链接、一个下划线色、中文与一个 emoji。
fn rich(shift: usize) -> Vec<StyledLine> {
    vec![
        StyledLine::new(vec![
            sp(" ".repeat(shift % 5)),
            sps("bold", Style::BOLD),
            sp(" and "),
            sps("dim", Style::DIM),
            sps(" ", Style::DEFAULT),
            sps("brand", Style::fg(BRAND)),
        ]),
        StyledLine::new(vec![
            sps("bg", Style::new(Color::Default, Color::Indexed(236), 0)),
            sp(" "),
            Span::with_decor(
                "under",
                Style::UNDERLINE,
                Decor::underline(Color::TrueColor(200, 40, 40)),
            ),
        ]),
        StyledLine::new(vec![
            sp("see "),
            Span::with_decor(
                "the spec",
                Style::UNDERLINE,
                Decor::link("https://example.com/spec"),
            ),
            sp(" there"),
        ]),
        StyledLine::new(vec![
            sp("中文".repeat(1 + shift % 3)),
            sp(" x "),
            sp("🙂 wide"),
            sp(" ".repeat(shift % 7)),
            sps("☀", Style::BOLD),
        ]),
        if shift.is_multiple_of(2) {
            StyledLine::new(vec![sps("even row", Style::ITALIC)])
        } else {
            StyledLine::EMPTY
        },
        StyledLine::new(vec![
            sp(" › "),
            sp(format!("draft{}", "a".repeat(shift % 9))),
            sps(" ", Style::CARET),
        ]),
    ]
}

fn renderer() -> Renderer {
    Renderer::new(String::new(), String::new())
}

/// 把这一帧（如果有）喂给解码器；它不认识的东西直接失败。
fn feed(vt: &mut Vt, frame: Option<String>) {
    if let Some(f) = frame {
        vt.feed(&f).unwrap_or_else(|e| panic!("{e}: {f:?}"));
    }
}

fn check(vt: &Vt, pool: &mut StylePool, lines: &[StyledLine], at: &str) {
    let want = expected(vt.width, vt.height, lines, pool);
    assert_eq!(vt.mismatch(&want, pool), None, "{at}");
    assert!(
        vt.caret_parked(&want),
        "{at}: cursor at ({}, {}), caret {:?}",
        vt.row,
        vt.col,
        want.caret
    );
}

#[test]
fn a_fresh_paint_decodes_to_the_frames_own_surface() {
    let _w = widths();
    let mut vt = Vt::new(W, H);
    let mut r = renderer();
    let mut pool = StylePool::new();
    feed(&mut vt, r.render(W, H, &rich(0), &[], None));
    check(&vt, &mut pool, &rich(0), "first frame");
    assert!(
        vt.last_synced,
        "the frame opens and closes synchronized output"
    );
    assert!(
        vt.cursor_hidden && vt.alt && vt.wrap_off,
        "alt screen, hidden cursor, autowrap off"
    );
    // shift 0："bold" 从第 0 列开始
    assert_eq!(
        *vt.cell(0, 0),
        VtCell {
            cp: 'b',
            style: Style::BOLD,
            decor: Decor::PLAIN,
            kind: WidthClass::Narrow
        }
    );
    assert_eq!(*vt.cell(0, 4), VtCell::BLANK);
    assert_eq!(vt.cell(5, 1).cp, '›');
}

#[test]
fn diff_frames_keep_the_grid_equal_to_each_new_surface() {
    let _w = widths();
    let mut vt = Vt::new(W, H);
    let mut r = renderer();
    let mut pool = StylePool::new();
    for i in 0..40 {
        feed(&mut vt, r.render(W, H, &rich(i), &[], None));
        check(&vt, &mut pool, &rich(i), &format!("frame {i}"));
    }
}

#[test]
fn a_resize_paints_the_new_size_whole() {
    let _w = widths();
    let mut vt = Vt::new(W, H);
    let mut r = renderer();
    let mut pool = StylePool::new();
    feed(&mut vt, r.render(W, H, &rich(1), &[], None));
    let mut wider = Vt::new(W + 7, H + 2);
    // 渲染器被告知新尺寸；字节去往那个尺寸的终端。
    let mut r2 = renderer();
    feed(&mut wider, r2.render(W + 7, H + 2, &rich(2), &[], None));
    check(&wider, &mut pool, &rich(2), "after resize");
}

#[test]
fn a_wide_glyph_that_does_not_fit_the_last_column_pads_and_stops() {
    let _w = widths();
    let mut vt = Vt::new(5, 1);
    let mut r = renderer();
    let mut pool = StylePool::new();
    let lines = vec![StyledLine::new(vec![sp("ab中文")])];
    feed(&mut vt, r.render(5, 1, &lines, &[], None));
    check(&vt, &mut pool, &lines, "clipped wide glyph");
    assert_eq!(vt.plain(), vec!["ab中 ".to_string()]); // 首格（跳过它的尾格），补的空格
}

#[test]
fn the_hash_follows_the_cells_not_the_frames_that_made_them() {
    let _w = widths();
    let mut a = Vt::new(W, H);
    let mut b = Vt::new(W, H);
    feed(&mut a, renderer().render(W, H, &rich(3), &[], None));
    let mut rb = renderer();
    feed(&mut b, rb.render(W, H, &rich(1), &[], None));
    feed(&mut b, rb.render(W, H, &rich(3), &[], None));
    assert_eq!(a.hash(), b.hash());
    assert_ne!(a.hash(), Vt::new(W, H).hash());
}

#[test]
fn a_removed_graphic_whose_left_edge_sits_on_a_wide_glyphs_spacer_rewrites_the_glyph() {
    // Rust 移植新加的回归：Scala 的 diff 在这里死循环（只有尾格变脏，从尾格开始的一段什么都不写、光标不前进）。
    let _w = widths();
    let mut vt = Vt::new(8, 2);
    let mut r = renderer();
    let mut pool = StylePool::new();
    let lines = vec![StyledLine::new(vec![sp("ab中文")]), StyledLine::EMPTY];
    // 一格宽的图形落在「中」的尾格（第 3 列）上。
    let pic = Graphic {
        id: 1,
        row: 0,
        col: 3,
        rows: 1,
        cols: 1,
        draw: "@".to_string(),
        erase: String::new(),
    };
    feed(&mut vt, r.render(8, 2, &lines, &[pic], None));
    feed(&mut vt, r.render(8, 2, &lines, &[], None));
    check(&vt, &mut pool, &lines, "graphic removed");
    assert_eq!(vt.plain()[0], "ab中文  ");
}
