//! 移植自 packages/tui/bench/test/src/zcode/tui/bench/VtSuite.scala（feat/tui a2bf878b34 加 bench 的改动）里不需要渲染器
//! 的部分；用渲染器的在 `render/renderer_tests.rs`。
use super::vt::*;
use crate::render::{Color, Decor, Style, WidthClass, attr};

const W: usize = 40;
const H: usize = 6;

#[test]
fn an_unknown_sequence_and_a_write_past_the_margin_fail_loudly() {
    let mut vt = Vt::new(W, H);
    let e = vt.feed("\x1b[5n").unwrap_err();
    assert!(e.to_string().contains("5n"), "{e}");
    let mut vt2 = Vt::new(3, 1);
    vt2.feed("abc").unwrap(); // 写满一行：光标停在最后一列，自动换行关着就是这样
    assert_eq!((vt2.row, vt2.col), (0, 2));
    assert!(vt2.feed("d").is_err());
    vt2.feed("\rxyz").unwrap(); // 回车重新对齐；这一行可以再写
    assert_eq!(vt2.plain(), vec!["xyz".to_string()]);
    assert!(vt2.feed("\x1b]9;unknown\u{7}").is_err());
}

/// kitty 的图像（APC `G`）跳过不画、不动光标；别的 APC 与没有结尾的 APC 是错（图像比的是屏幕的 `graphics`）。
#[test]
fn a_kitty_image_is_skipped_and_any_other_apc_fails() {
    let mut vt = Vt::new(W, H);
    vt.feed("ab\x1b_Ga=T,f=100;AAAA\x1b\\cd").unwrap();
    assert_eq!(vt.plain()[0].trim_end(), "abcd");
    assert!(vt.feed("\x1b_Xzz\x1b\\").is_err());
    assert!(vt.feed("\x1b_Ga=d").is_err());
}

#[test]
fn sgr_forms_named_indexed_truecolour_attributes_underline_colour_reset_keeps_the_link() {
    let mut vt = Vt::new(W, 1);
    vt.feed("\x1b[0;1;3;9;31;48;5;17mA\x1b[0;38;2;1;2;3;104m\x1b[58:2::9:8:7mB\x1b]8;;https://x\x1b\\\x1b[0mC\x1b]8;;\x1b\\D").unwrap();
    assert_eq!(
        *vt.cell(0, 0),
        VtCell {
            cp: 'A',
            style: Style::new(
                Color::Named(1),
                Color::Indexed(17),
                attr::BOLD | attr::ITALIC | attr::STRIKE
            ),
            decor: Decor::PLAIN,
            kind: WidthClass::Narrow
        }
    );
    assert_eq!(
        *vt.cell(0, 1),
        VtCell {
            cp: 'B',
            style: Style::new(Color::TrueColor(1, 2, 3), Color::Named(12), 0),
            decor: Decor::underline(Color::TrueColor(9, 8, 7)),
            kind: WidthClass::Narrow
        }
    );
    assert_eq!(
        *vt.cell(0, 2),
        VtCell {
            cp: 'C',
            style: Style::DEFAULT,
            decor: Decor::link("https://x"),
            kind: WidthClass::Narrow
        }
    );
    assert_eq!(
        *vt.cell(0, 3),
        VtCell {
            cp: 'D',
            style: Style::DEFAULT,
            decor: Decor::PLAIN,
            kind: WidthClass::Narrow
        }
    );
}

#[test]
fn the_hash_is_fnv1a_64_over_the_canonical_cell_text() {
    // 规范文本与 Scala 的 VtSuite 相同；空屏的哈希两边钉的是同一个常量。
    let mut vt = Vt::new(3, 1);
    vt.feed("\x1b[1;38;2;72;160;255mA\x1b[0;4m\x1b[58:5:9m\x1b]8;;https://x\x1b\\\u{4e2d}")
        .unwrap();
    assert_eq!(canonical(vt.cell(0, 0)), "65;1;t72,160,255;d;d;;0\n");
    assert_eq!(canonical(vt.cell(0, 1)), "20013;8;d;d;i9;https://x;1\n");
    assert_eq!(canonical(vt.cell(0, 2)), "32;8;d;d;i9;https://x;2\n");
    assert_eq!(vt.hash().len(), 16);
    assert_eq!(Vt::new(1, 1).hash(), hash_of(&[VtCell::BLANK]));
    assert_eq!(hash_of(&[VtCell::BLANK]), PINNED_BLANK_HASH);
}

#[test]
fn cursor_moves_erase_and_clear() {
    let mut vt = Vt::new(4, 2);
    vt.feed("ab\x1b[2;1Hcd\x1b[1A\x1b[1Dx\x1b[0K").unwrap();
    assert_eq!(vt.plain(), vec!["ax  ".to_string(), "cd  ".to_string()]);
    vt.feed("\x1b[2J").unwrap();
    assert_eq!(vt.plain(), vec!["    ".to_string(), "    ".to_string()]);
    assert!(vt.feed("\x1b[3;1H").is_err());
}
