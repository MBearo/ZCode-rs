//! 改动片段：期望值用 jsdiff 9 按同样的写法算出（`structuredPatch`，上下文 8 行，去掉删除行后从旧文件的起始行编号）。

use super::snippet;

fn lines(count: usize, line: impl Fn(usize) -> String) -> String {
    (1..=count).map(|n| line(n) + "\n").collect()
}

#[test]
fn a_changed_line_comes_with_its_context() {
    assert_eq!(snippet("a\nb\nc\n", "a\nB\nc\n"), "1\ta\n2\tB\n3\tc");
    assert_eq!(snippet("x\n", "x\ny\n"), "1\tx\n2\ty");
    assert_eq!(snippet("", "new\n"), "1\tnew");
    assert_eq!(
        snippet("a\nb\nc\nd\n", "d\nc\nb\na\n"),
        "1\td\n2\tc\n3\tb\n4\ta"
    );
}

/// 只差结尾的换行时，hunk 里的行都是上下文与新行（“No newline” 的标记不显示）。
#[test]
fn a_missing_final_newline_shows_the_lines() {
    assert_eq!(snippet("a\nb\n", "a\nb"), "1\ta\n2\tb");
}

/// 没有改动、只删了行（没有可显示的行）时为空。
#[test]
fn nothing_to_show_is_empty() {
    assert_eq!(snippet("", ""), "");
    assert_eq!(snippet("same\n", "same\n"), "");
    assert_eq!(snippet("a\n", ""), "");
}

/// 相隔超过 16 行的两处改动是两个 hunk，中间是 `...`。
#[test]
fn far_apart_changes_are_separate_hunks() {
    let old = lines(25, |n| n.to_string());
    let new = old.replacen("2\n", "2x\n", 1).replacen("24\n", "24x\n", 1);
    assert_eq!(
        snippet(&old, &new),
        "1\t1\n2\t2x\n3\t3\n4\t4\n5\t5\n6\t6\n7\t7\n8\t8\n9\t9\n10\t10\n...\n16\t16\n17\t17\n18\t18\n19\t19\n20\t20\n21\t21\n22\t22\n23\t23\n24\t24x\n25\t25"
    );
}

/// 编号按旧文件的起始行连续往下数：插入的行占一个号，删掉的行不占。
#[test]
fn numbering_continues_from_the_old_start() {
    let old = lines(40, |n| format!("k{n}"));
    let new = old
        .replacen("k5\n", "k5\nNEW\n", 1)
        .replacen("k30\n", "", 1);
    assert_eq!(
        snippet(&old, &new),
        "1\tk1\n2\tk2\n3\tk3\n4\tk4\n5\tk5\n6\tNEW\n7\tk6\n8\tk7\n9\tk8\n10\tk9\n11\tk10\n12\tk11\n13\tk12\n14\tk13\n...\n22\tk22\n23\tk23\n24\tk24\n25\tk25\n26\tk26\n27\tk27\n28\tk28\n29\tk29\n30\tk31\n31\tk32\n32\tk33\n33\tk34\n34\tk35\n35\tk36\n36\tk37\n37\tk38"
    );
}

#[test]
fn non_ascii_text_is_kept() {
    assert_eq!(snippet("你好\n😀\n", "你好\n😀!\n"), "1\t你好\n2\t😀!");
}

/// 超过 8192 个 UTF-16 单元：截在之前的最后一个换行，接上没显示的行数。
#[test]
fn long_snippets_are_truncated_at_a_line() {
    let old = lines(30, |n| format!("old {n}"));
    let new = lines(2000, |n| format!("line {n}"));
    let found = snippet(&old, &new);
    assert_eq!(found.encode_utf16().count(), 8213);
    assert!(found.starts_with("1\tline 1\n2\tline 2\n"), "{found}");
    assert!(
        found.ends_with("645\tline 645\n646\tline 646\n\n... [1354 lines truncated] ..."),
        "{}",
        &found[found.len() - 80..]
    );
}
