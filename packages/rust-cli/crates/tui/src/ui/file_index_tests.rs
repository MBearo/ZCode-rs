use super::*;

fn index(files: &[&str]) -> FileIndex {
    FileIndex::new(files.iter().map(|f| (*f).to_owned()).collect(), '/')
}

#[test]
fn directories_come_before_files_and_end_with_a_separator() {
    let index = index(&["src/ui/app.rs", "src/lib.rs", "README.md"]);
    assert_eq!(index.len(), 5);
    assert_eq!(index.search("", 10), ["src/", "README.md"]);
}

#[test]
fn boundary_and_consecutive_matches_rank_first() {
    let index = index(&[
        "docs/specs/app-notes.md",
        "src/ui/app.rs",
        "packages/a/p/p.rs",
        "src/apply.rs",
    ]);
    let found = index.search("app", 3);
    // `app` 连续且在边界上的在前；短路径加分。
    assert_eq!(found[0], "src/ui/app.rs");
    assert!(found.contains(&"src/apply.rs".to_owned()));
    assert!(!found.contains(&"packages/a/p/p.rs".to_owned()) || found.len() == 3);
}

#[test]
fn smart_case_and_letter_filter() {
    let index = index(&["src/FileIndex.ts", "src/fileindex.md"]);
    assert_eq!(index.search("FI", 5), ["src/FileIndex.ts"]);
    assert_eq!(index.search("fi", 5).len(), 2);
    assert!(index.search("zzz", 5).is_empty());
}

#[test]
fn limit_caps_the_results() {
    let files: Vec<String> = (0..40).map(|i| format!("dir/file{i}.txt")).collect();
    let index = FileIndex::new(files, '/');
    assert_eq!(index.search("file", MAX_SUGGESTIONS).len(), MAX_SUGGESTIONS);
}
