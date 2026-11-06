//! `@` 面板的组（照 Puffin `shell/at_tests.rs` 里文件的那几组）。
use rust_cli_tui_kit::app::widgets::at_panel::data::ConversationRef;
use rust_cli_tui_kit::app::widgets::at_panel::{AtItem, Touch, name_of};

use super::*;
use crate::ui::at_search::{FileHit, FileSearch};

fn answer(query: &str, under: &str, hits: &[(&str, bool)]) -> AtFiles {
    AtFiles {
        query: query.to_owned(),
        under: under.to_owned(),
        found: FileSearch {
            hits: hits
                .iter()
                .map(|(p, folder)| FileHit {
                    path: (*p).to_owned(),
                    folder: *folder,
                })
                .collect(),
            total: hits.len() as u64,
        },
    }
}

fn sources<'a>(files: Option<&'a AtFiles>, touched: &'a [(String, Touch)]) -> Sources<'a> {
    Sources {
        files,
        touched,
        changed: &[],
        conversations: None,
        current: None,
        lang: Lang::En,
    }
}

fn talk(id: &str, title: &str, updated_at: u64) -> ConversationRef {
    ConversationRef {
        session_id: id.into(),
        title: title.into(),
        updated_at,
    }
}

#[test]
fn conversations_follow_the_files_newest_first_without_the_current_one() {
    let found = answer("", "", &[("a.rs", false)]);
    let talks = [
        talk("sess_old", "Fix the footer", 1),
        talk("sess_now", "This one", 3),
        talk("sess_new", "Port the panel", 2),
    ];
    let s = Sources {
        conversations: Some(&talks),
        current: Some("sess_now"),
        ..sources(Some(&found), &[])
    };
    let p = panel("", &s, MAX_ROWS);
    let labels: Vec<&str> = p.groups.iter().map(|g| g.label.as_str()).collect();
    assert_eq!(labels, ["Files", "Conversations"]);
    let titles: Vec<String> = p.groups[1].items.iter().map(name_of).collect();
    assert_eq!(titles, ["Port the panel", "Fix the footer"]);
    // 有筛选词时按标题排序；多出的写 more。
    let found = answer("foot", "", &[]);
    let s = Sources {
        conversations: Some(&talks),
        current: Some("sess_now"),
        ..sources(Some(&found), &[])
    };
    let p = panel("foot", &s, MAX_ROWS);
    assert_eq!(p.groups.len(), 1);
    assert_eq!(p.groups[0].label, "Conversations");
    assert_eq!(name_of(&p.groups[0].items[0]), "Fix the footer");
}

fn names(p: &AtPanel) -> Vec<String> {
    p.entries()
        .iter()
        .map(|it| match it {
            AtItem::Path { path, .. } => path.clone(),
            _ => String::new(),
        })
        .collect()
}

#[test]
fn before_the_files_arrive_the_panel_is_looking() {
    let p = panel("foot", &sources(None, &[]), MAX_ROWS);
    assert_eq!(p.groups.len(), 1);
    assert!(p.groups[0].loading);
    assert_eq!(p.groups[0].label, "Files");
}

#[test]
fn a_word_lists_four_files_in_the_answers_order_and_counts_the_rest() {
    let found = answer(
        "foot",
        "",
        &[
            ("src/footer.rs", false),
            ("src/footer_tests.rs", false),
            ("docs/footer.md", false),
            ("src/footer", true),
            ("src/footnote.rs", false),
            ("src/foot.rs", false),
        ],
    );
    let p = panel("foot", &sources(Some(&found), &[]), MAX_ROWS);
    assert_eq!(
        names(&p),
        [
            "src/footer.rs",
            "src/footer_tests.rs",
            "docs/footer.md",
            "src/footer"
        ]
    );
    assert_eq!(
        (p.groups[0].more, p.groups[0].more_word.as_str()),
        (2, "files")
    );
}

#[test]
fn files_this_conversation_touched_come_first_when_only_at_is_typed() {
    let found = answer("", "", &[("a.rs", false), ("b.rs", false)]);
    let touched = [("c.rs".to_owned(), Touch::Edited)];
    let p = panel("", &sources(Some(&found), &touched), MAX_ROWS);
    assert_eq!(p.groups[0].label, "Recent files");
    assert_eq!(names(&p), ["c.rs"]);
    let p = panel("", &sources(Some(&found), &[]), MAX_ROWS);
    assert_eq!(p.groups[0].label, "Files");
    assert_eq!(names(&p), ["a.rs", "b.rs"]);
}

#[test]
fn a_typed_path_lists_what_is_below_it_folders_first() {
    let found = answer(
        "",
        "src/",
        &[
            ("src/ui/", true),
            ("src/a.rs", false),
            ("src/ui/x.rs", false),
        ],
    );
    let p = panel("src/", &sources(Some(&found), &[]), MAX_ROWS);
    assert!(p.single());
    assert_eq!(p.groups[0].label, "src/");
    assert_eq!(names(&p), ["src/ui/", "src/a.rs"]);
    let found = answer("a", "src/", &[("src/a.rs", false), ("src/ab/", true)]);
    let p = panel("src/a", &sources(Some(&found), &[]), MAX_ROWS);
    assert_eq!(p.groups[0].label, "Files in src/");
    assert_eq!(names(&p), ["src/ab/", "src/a.rs"]);
}

#[test]
fn an_edit_after_a_read_counts_as_edited_and_paths_outside_are_dropped() {
    let read = serde_json::json!({"file_path": "/w/src/a.rs"});
    let edit = serde_json::json!({"file_path": "/w/src/a.rs"});
    let outside = serde_json::json!({"file_path": "/elsewhere/b.rs"});
    let tools = [("Read", &read), ("Edit", &edit), ("Read", &outside)];
    assert_eq!(
        touched(tools.iter().map(|(n, i)| (*n, *i)), "/w"),
        [("src/a.rs".to_owned(), Touch::Edited)]
    );
}

#[test]
fn chinese_labels_keep_the_layout() {
    let found = answer("x", "", &[("x.rs", false)]);
    let s = Sources {
        lang: Lang::Zh,
        ..sources(Some(&found), &[])
    };
    assert_eq!(panel("x", &s, MAX_ROWS).groups[0].label, "文件");
    assert_eq!(panel("y/x", &s, MAX_ROWS).groups[0].label, "y/ 里的文件");
}
