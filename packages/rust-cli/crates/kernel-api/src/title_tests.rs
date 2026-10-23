//! 首条输入的临时标题（Node `titleFromInput`）。
use super::*;

#[test]
fn first_input_titles_follow_node() {
    assert_eq!(from_input("  fix   the\n\tbug  "), "fix the bug");
    assert_eq!(from_input(" \n "), "Untitled session");
    let long = "a".repeat(61);
    assert_eq!(from_input(&long), format!("{}...", "a".repeat(57)));
    assert_eq!(from_input(&"b".repeat(60)), "b".repeat(60));
}
