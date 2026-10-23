// 复制自 rust/crates/kit/src/js/string_tests.rs（feat/rs 79b6691c55）。
use super::*;

#[test]
fn sanitizes_like_node() {
    assert_eq!(sanitize_message("  a \n\t b\u{feff}"), Some("a b".into()));
    assert_eq!(sanitize_message(" \n "), None);
    let long = "字".repeat(600);
    let cut = sanitize_message(&long).unwrap();
    assert_eq!(cut.chars().count(), 500);
    assert!(cut.ends_with("..."));
    assert_eq!(utf16_prefix("a😀b", 2), "a");
    assert_eq!(trim("\u{85}x\u{feff}"), "\u{85}x");
}

#[test]
fn js_whitespace_is_rust_whitespace_without_u85_and_with_bom() {
    // 以前 kernel-api 的 `js_trim` 用这个式子，policy 的两份写成字符表；三者与 `is_space` 逐个相同（F58）。
    for c in (0..=0x10_ffff).filter_map(char::from_u32) {
        let expected = (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}';
        assert_eq!(is_space(c), expected, "U+{:04X}", u32::from(c));
    }
}

#[test]
fn lengths_count_utf16_units() {
    assert_eq!(utf16_len(""), 0);
    assert_eq!(utf16_len("a😀b"), 4);
    for text in ["", "abc", "字", "a😀b\u{feff}"] {
        assert_eq!(
            utf16_len(text),
            text.chars().map(char::len_utf16).sum::<usize>()
        );
    }
}

#[test]
fn spaces_collapse_like_js() {
    assert_eq!(collapse_spaces("  a \n\t b\u{feff}c  "), "a b c");
    assert_eq!(collapse_spaces("\u{85}x"), "\u{85}x");
    assert_eq!(collapse_spaces(" \u{3000} "), "");
}
