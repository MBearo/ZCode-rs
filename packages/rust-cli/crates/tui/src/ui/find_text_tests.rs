//! 搜索的匹配规则（docs/specs/rust-cli/tui-input.md §2.3.3）：大小写、字素、宽字符、不重叠。
use super::*;

fn find(line: &str, query: &str) -> Vec<(usize, usize)> {
    Folded::new(line).find(&fold(query))
}

#[test]
fn case_is_ignored_and_columns_are_display_columns() {
    assert_eq!(
        find("Error: error ERROR", "error"),
        [(0, 5), (7, 12), (13, 18)]
    );
    assert_eq!(find("Ünïcode ünïCODE", "ÜNÏ"), [(0, 3), (8, 11)]);
    assert!(find("abc", "").is_empty());
    assert!(find("abc", "abcd").is_empty());
}

#[test]
fn wide_characters_take_two_columns() {
    assert_eq!(find("第一行写着搜索的词", "搜索"), [(10, 14)]);
    assert_eq!(find("a中b", "B"), [(3, 4)]);
}

/// 匹配的两端都要在字素边界上：分解写法的 `é`（e + U+0301）里的 `e` 不算；整个字素算。
#[test]
fn matches_do_not_split_graphemes() {
    let decomposed = "cafe\u{301} cafe";
    assert_eq!(find(decomposed, "cafe"), [(5, 9)]);
    assert_eq!(find(decomposed, "cafe\u{301}"), [(0, 4)]);
    // 转小写后变长的字符（`İ` → `i̇`）：整行照样能搜，其余的列不错位。
    assert_eq!(find("İstanbul istanbul", "istanbul"), [(9, 17)]);
    assert_eq!(find("İstanbul", "İst"), [(0, 3)]);
}

#[test]
fn occurrences_do_not_overlap() {
    assert_eq!(find("aaaa", "aa"), [(0, 2), (2, 4)]);
}

#[test]
fn backspace_removes_one_grapheme() {
    let mut query = "ae\u{301}".to_owned();
    pop_grapheme(&mut query);
    assert_eq!(query, "a");
    let mut query = "搜索".to_owned();
    pop_grapheme(&mut query);
    assert_eq!(query, "搜");
    let mut empty = String::new();
    pop_grapheme(&mut empty);
    assert!(empty.is_empty());
}
