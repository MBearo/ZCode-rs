//! Node `cleanGeneratedTitle` / `normalizeTitleInput` 的各分支（titles.md §3）。
use super::*;

#[test]
fn json_fenced_and_thinking_replies_are_parsed() {
    assert_eq!(
        clean_title(r#"{"title":"Session titles"}"#).as_deref(),
        Some("Session titles")
    );
    assert_eq!(
        clean_title("<think>hmm</think>\n```json\n{\"title\": \"Hook trust digest\"}\n```")
            .as_deref(),
        Some("Hook trust digest")
    );
    assert_eq!(
        clean_title("<THINK>a</THINK><think>b</think> {\"title\":\"x y\"}").as_deref(),
        Some("x y")
    );
    // 不是 JSON：第一个非空行，去掉 #、引号、末尾标点，空白压成一个。
    assert_eq!(
        clean_title("\n\r\n## “Config  files。”\nsecond").as_deref(),
        Some("Config files")
    );
    // 先去引号再去末尾标点（Node 的顺序）：引号在标点前面时留下。
    assert_eq!(
        clean_title("“Config files”。").as_deref(),
        Some("Config files”")
    );
    assert_eq!(
        clean_title("'Fix: the bug?!'").as_deref(),
        Some("Fix: the bug")
    );
    assert_eq!(
        clean_title("配置文件与信任").as_deref(),
        Some("配置文件与信任")
    );
}

#[test]
fn empty_or_meaningless_titles_are_dropped_and_long_ones_cut() {
    assert_eq!(clean_title(""), None);
    assert_eq!(clean_title("<think>only thinking</think>"), None);
    assert_eq!(clean_title("“…”"), None);
    assert_eq!(
        clean_title(r#"{"title": 3}"#).as_deref(),
        Some("{\"title\": 3}")
    );
    let long = format!("{} tail", "a".repeat(120));
    let cut = clean_title(&long).unwrap();
    assert_eq!(cut, format!("{}...", "a".repeat(97)));
}

#[test]
fn inputs_are_collapsed_and_capped() {
    assert_eq!(normalize_input("  fix\n\n the   bug "), "fix the bug");
    assert_eq!(normalize_input(&"x".repeat(1300)).len(), 1200);
}
