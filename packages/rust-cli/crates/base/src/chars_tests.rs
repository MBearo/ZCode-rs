use super::*;

/// 收拢前 tools `common::escape_control` 的判断（逐字照抄）。
fn old_escaped(c: char) -> bool {
    let code = u32::from(c);
    code <= 0x1f || (0x7f..=0x9f).contains(&code) || code == 0x2028 || code == 0x2029
}

#[test]
fn entity_escaping_matches_the_old_rule_for_every_char() {
    for code in 0..=0x10ffff_u32 {
        if let Some(c) = char::from_u32(code) {
            assert_eq!(is_entity_escaped(c), old_escaped(c), "U+{code:04X}");
        }
    }
    assert_eq!(
        escape_entities("a\u{1}b\u{7f}\u{2028}é\n"),
        "a&#1;b&#127;&#8232;é&#10;"
    );
}

#[test]
fn control_ranges() {
    assert!(is_c0('\0') && is_c0('\u{1f}') && !is_c0(' '));
    assert!(is_del_or_c1('\u{7f}') && is_del_or_c1('\u{9f}') && !is_del_or_c1('\u{a0}'));
    assert!(is_line_break_separator('\u{2028}') && is_line_break_separator('\u{2029}'));
    assert!(!is_line_break_separator('\n'));
}

#[test]
fn bidi_and_zero_width_is_the_union_of_the_three_old_sets() {
    let inside = [
        0x061c, 0x200b, 0x200c, 0x200d, 0x200e, 0x200f, 0x202a, 0x202e, 0x2066, 0x2069, 0x206a,
        0x206f, 0xfeff,
    ];
    for code in inside {
        assert!(
            is_bidi_or_zero_width(char::from_u32(code).unwrap()),
            "U+{code:04X}"
        );
    }
    for code in [
        0x061b, 0x200a, 0x2010, 0x2029, 0x202f, 0x2065, 0x2070, 0xfe0f, 0xfefe, 0x41,
    ] {
        assert!(
            !is_bidi_or_zero_width(char::from_u32(code).unwrap()),
            "U+{code:04X}"
        );
    }
}
