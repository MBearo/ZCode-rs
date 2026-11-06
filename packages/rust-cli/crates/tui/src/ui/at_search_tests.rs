//! `@` 面板的文件搜索（照 Puffin 的 host 的 `mentions_tests.rs`）。
use super::*;

fn list(paths: &[&str]) -> Vec<FileHit> {
    entries(
        &paths.iter().map(|p| (*p).to_owned()).collect::<Vec<_>>(),
        '/',
    )
}

fn paths(hits: &[FileHit]) -> Vec<&str> {
    hits.iter().map(|h| h.path.as_str()).collect()
}

#[test]
fn folders_come_from_the_paths_and_the_windows_separator_becomes_a_slash() {
    let hits = entries(&["src\\ui\\a.rs".to_owned(), "b.md".to_owned()], '\\');
    assert_eq!(paths(&hits), ["src/", "src/ui/", "src/ui/a.rs", "b.md"]);
    assert!(hits[0].folder && !hits[2].folder);
}

#[test]
fn a_name_match_beats_a_path_match_beats_letters_in_order() {
    let all = list(&["foot/x.rs", "src/footer.rs", "docs/f_o_o_t.md"]);
    assert_eq!(
        paths(&search(&all, "foot", "")),
        ["foot/", "src/footer.rs", "foot/x.rs", "docs/f_o_o_t.md"]
    );
}

#[test]
fn an_empty_word_lists_the_folder_itself_folders_first() {
    let all = list(&["src/b.rs", "src/a/x.rs", "README.md"]);
    assert_eq!(paths(&search(&all, "", "")), ["src/", "README.md"]);
    assert_eq!(paths(&search(&all, "", "src/")), ["src/a/", "src/b.rs"]);
}

#[test]
fn git_renames_keep_the_new_path() {
    let numstat = "1\t2\tsrc/{old => new}/a.rs\n3\t0\tb.rs => c.rs\n";
    assert_eq!(
        changed_paths(numstat, "notes.txt\n../out.txt\n"),
        ["src/new/a.rs", "c.rs", "notes.txt"]
    );
}
