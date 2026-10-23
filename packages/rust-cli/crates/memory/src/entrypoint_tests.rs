use super::*;

#[test]
fn short_indexes_are_only_trimmed() {
    assert_eq!(truncate("\n- [A](a.md) — hook\n\n"), "- [A](a.md) — hook");
}

#[test]
fn too_many_lines_keep_the_first_200() {
    let raw: Vec<String> = (1..=205).map(|n| format!("- line {n}")).collect();
    let out = truncate(&raw.join("\n"));
    let (kept, warning) = out.split_once("\n\n> WARNING: ").unwrap();
    assert_eq!(kept.lines().count(), 200);
    assert!(kept.ends_with("- line 200"));
    assert_eq!(
        warning,
        "MEMORY.md is 205 lines (limit: 200). Only part of it was loaded: 5 of 205 lines were cut off, starting at line 201 (\"- line 201\"). Keep index entries to one line under ~200 chars; move detail into topic files."
    );
}

#[test]
fn too_long_cuts_at_the_last_newline_before_the_limit() {
    let line = "x".repeat(999);
    let raw: Vec<String> = (0..30).map(|_| line.clone()).collect();
    let out = truncate(&raw.join("\n"));
    let (kept, warning) = out.split_once("\n\n> WARNING: ").unwrap();
    // 每行 1000 码元（含换行）：25000 处正好是第 25 行末尾的换行。
    assert_eq!(kept.lines().count(), 25);
    assert_eq!(
        warning,
        "MEMORY.md is 29.3KB (limit: 24.4KB) — index entries are too long. Only part of it was loaded: 5 of 30 lines were cut off, starting at line 26 (\"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx…\"). Keep index entries to one line under ~200 chars; move detail into topic files."
    );
}

#[test]
fn a_huge_first_line_is_cut_inside_line_one() {
    let out = truncate(&"y".repeat(30_000));
    assert!(out.starts_with(&"y".repeat(25_000)));
    assert!(out.ends_with("MEMORY.md is 29.3KB (limit: 24.4KB) — index entries are too long. Only part of it was loaded: everything after the first 25000 characters of line 1 was cut off. Keep index entries to one line under ~200 chars; move detail into topic files."));
}

#[test]
fn bytes_format_with_one_decimal_kb() {
    assert_eq!(format_bytes(500), "500 bytes");
    assert_eq!(format_bytes(25_000), "24.4KB");
    assert_eq!(format_bytes(2048), "2KB");
}

#[test]
fn sizes_round_ties_up_like_to_fixed() {
    // 25856 码元 = 25.25 KB：`formatBytes` 用 JS `toFixed(1)`，平分时取大；F58 之前这里用 Rust 的 `{:.1}`
    // （平分时取偶），得 25.2KB。
    let out = truncate(&"z".repeat(25_856));
    assert!(out.contains("MEMORY.md is 25.3KB (limit: 24.4KB)"), "{out}");
}
