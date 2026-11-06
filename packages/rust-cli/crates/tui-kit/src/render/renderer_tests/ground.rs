//! 主题涂的地（docs/puffin-tui.md §4.2「谁涂地」）。
//! 移植自 packages/tui/tui/test/src/zcode/tui/render/RendererSuite.scala（feat/tui a2bf878b34）。
use super::{Rig, lines};
use crate::render::renderer::Backdrop;
use crate::render::{Color, Span, Style, StyledLine, ansi};

const INK: Color = Color::TrueColor(0xeb, 0xdb, 0xb2);
const GROUND: Color = Color::TrueColor(0x28, 0x28, 0x28);
const ORIGINAL: (Color, Color) = (
    Color::TrueColor(0xd6, 0xd8, 0xdc),
    Color::TrueColor(0x15, 0x16, 0x1a),
);
const PAINTED: Backdrop = Backdrop {
    fg: INK,
    bg: GROUND,
    restore: Some(ORIGINAL),
};
/// 同样的颜色，还法换成终端的缺省（外观变过之后）。
const PAINTED_NO_RESTORE: Backdrop = Backdrop {
    fg: INK,
    bg: GROUND,
    restore: None,
};

fn base() -> String {
    Style::new(INK, GROUND, 0).set_sequence()
}

#[test]
fn a_full_width_fill_reaches_the_terminal_with_no_background_on_its_edge_cells() {
    let mut t = Rig::new(4, 1);
    let panel = Color::rgb(33, 34, 38);
    t.paint(
        4,
        1,
        &[StyledLine::text("    ", Style::bg(panel))],
        &[],
        None,
    );
    assert_eq!(t.emu.line(0), "█  █");
    assert!(
        t.last
            .contains(&format!("{}█", Style::fg(panel).set_sequence())),
        "{:?}",
        t.last
    );
}

#[test]
fn a_painted_ground_writes_uncoloured_cells_in_its_colours_and_erases_on_it() {
    let mut t = Rig::new(20, 3);
    let line = StyledLine::new(vec![
        Span::new("a", Style::DEFAULT),
        Span::new("b", Style::fg(Color::RED)),
    ]);
    t.paint(20, 3, &[line], &[], Some(&PAINTED));
    let frame = &t.last;
    // 终端的默认色设成主题的，边距也是主题的颜色。
    assert!(frame.contains(&ansi::set_colors(INK, GROUND)), "{frame:?}");
    assert!(
        frame.contains("\x1b]10;rgb:eb/db/b2\u{7}\x1b]11;rgb:28/28/28\u{7}"),
        "{frame:?}"
    );
    // 清屏之前先设上地的颜色：擦掉的格子就是地。
    assert!(
        frame.contains(&format!("{}{}", base(), ansi::CLEAR_SCREEN)),
        "{frame:?}"
    );
    // 有颜色的格子只换掉它没写的那一半。
    assert!(
        frame.contains(&format!(
            "{}b",
            Style::new(Color::RED, GROUND, 0).set_sequence()
        )),
        "{frame:?}"
    );
    assert_eq!(t.emu.line(0), "ab");
}

#[test]
fn without_a_backdrop_nothing_changes_plain_resets_no_osc_10_11() {
    let mut t = Rig::new(20, 2);
    t.render(20, 2, &lines(&["a"]));
    assert!(
        !t.last.contains("\x1b]10;") && !t.last.contains("\x1b]11;"),
        "{:?}",
        t.last
    );
    assert!(
        t.last
            .contains(&format!("{}{}", ansi::RESET, ansi::CLEAR_SCREEN)),
        "{:?}",
        t.last
    );
}

#[test]
fn a_width_disputed_row_is_erased_on_the_ground_not_on_the_terminals_colours() {
    let mut t = Rig::new(20, 2);
    t.paint(20, 2, &lines(&["a"]), &[], Some(&PAINTED));
    t.paint(20, 2, &lines(&["✅ x"]), &[], Some(&PAINTED));
    // 整行重写从地的颜色开始，擦除时仍是它。
    assert!(
        t.last
            .contains(&format!("{}✅ x{}", base(), ansi::ERASE_TO_EOL)),
        "{:?}",
        t.last
    );
}

#[test]
fn a_full_width_disputed_row_keeps_its_last_cell_on_the_painted_ground() {
    // 两处改动在这里相遇：整行写满时不擦行尾、把最后一个字形再写一次；整行从地的颜色开始。
    let mut t = Rig::new(24, 2);
    let full = format!("✅{}", "x".repeat(22)); // 按 Width 正好 24 列
    t.paint(24, 2, &lines(&["a"]), &[], Some(&PAINTED));
    t.paint(24, 2, &lines(&[&full]), &[], Some(&PAINTED));
    assert_eq!(t.emu.line(0), full);
    assert!(
        t.last.contains(&format!("{}{full}", base())),
        "{:?}",
        t.last
    );
    let again = format!(
        "{}{}23Cx{}",
        ansi::CARRIAGE_RETURN,
        ansi::CSI,
        ansi::CARRIAGE_RETURN
    );
    assert!(t.last.contains(&again), "{:?}", t.last);
    assert!(!t.last.contains(ansi::ERASE_TO_EOL), "{:?}", t.last);
}

#[test]
fn changing_the_ground_repaints_the_whole_screen_in_the_new_colours() {
    let mut t = Rig::new(20, 2);
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED));
    let light = Backdrop {
        fg: Color::TrueColor(0x3c, 0x38, 0x36),
        bg: Color::TrueColor(0xfb, 0xf1, 0xc7),
        restore: Some(ORIGINAL),
    };
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&light));
    assert!(
        t.last.contains(&ansi::set_colors(light.fg, light.bg)),
        "{:?}",
        t.last
    );
    assert!(
        t.last
            .contains(&Style::new(light.fg, light.bg, 0).set_sequence()),
        "{:?}",
        t.last
    );
    assert!(
        t.last.contains(ansi::CLEAR_SCREEN) && t.last.contains("hi"),
        "{:?}",
        t.last
    );
    assert_eq!(t.emu.line(0), "hi");
}

#[test]
fn a_new_restore_with_the_same_colours_writes_nothing() {
    let mut t = Rig::new(20, 2);
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED));
    let before = t.writes;
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED_NO_RESTORE));
    assert_eq!(t.writes, before);
}

#[test]
fn leaving_the_alt_buffer_gives_the_terminal_its_probed_colours_back_before_1049l() {
    let mut t = Rig::new(20, 2);
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED));
    t.exit();
    assert!(
        t.last
            .starts_with(&ansi::set_colors(ORIGINAL.0, ORIGINAL.1)),
        "{:?}",
        t.last
    );
    assert!(t.last.ends_with(ansi::ALT_SCREEN_EXIT), "{:?}", t.last);
}

#[test]
fn after_an_appearance_change_it_gives_back_the_terminals_own_defaults_osc_110_111() {
    let mut t = Rig::new(20, 2);
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED));
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED_NO_RESTORE));
    t.exit();
    assert!(t.last.starts_with(ansi::RESET_COLORS), "{:?}", t.last);
    // 退出时不再问颜色：回答会落进 shell。
    assert!(!t.last.contains(ansi::ASK_COLORS), "{:?}", t.last);
}

#[test]
fn dropping_the_ground_mid_session_gives_the_colours_back_and_repaints_on_the_terminals() {
    let mut t = Rig::new(20, 2);
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED));
    t.render(20, 2, &lines(&["hi"]));
    assert!(
        t.last.contains(&ansi::set_colors(ORIGINAL.0, ORIGINAL.1)),
        "{:?}",
        t.last
    );
    let repaint = format!("{}{}{}", ansi::RESET, ansi::CURSOR_HOME, ansi::CLEAR_SCREEN);
    assert!(t.last.contains(&repaint), "{:?}", t.last);
    assert_eq!(t.emu.line(0), "hi");
    drop(t);
    // 还成终端的默认时再问一次颜色：之后的主题要用终端现在的颜色。
    let mut t2 = Rig::new(20, 2);
    t2.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED_NO_RESTORE));
    t2.render(20, 2, &lines(&["hi"]));
    let ask = format!("{}{}", ansi::RESET_COLORS, ansi::ASK_COLORS);
    assert!(t2.last.contains(&ask), "{:?}", t2.last);
}

#[test]
fn coming_back_to_the_alt_buffer_sets_the_ground_again() {
    let mut t = Rig::new(20, 2);
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED));
    t.exit();
    t.paint(20, 2, &lines(&["hi"]), &[], Some(&PAINTED));
    assert!(t.last.contains(ansi::ALT_SCREEN_ENTER), "{:?}", t.last);
    assert!(
        t.last.contains(&ansi::set_colors(INK, GROUND)),
        "{:?}",
        t.last
    );
    assert!(
        t.last
            .contains(&format!("{}{}", base(), ansi::CLEAR_SCREEN)),
        "{:?}",
        t.last
    );
}
