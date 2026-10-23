use super::*;

fn state(content: &str, version: &str) -> FileState {
    FileState {
        content: content.to_owned(),
        baseline: Baseline::Version(FileVersion::new(version)),
        offset: Some(1),
        limit: None,
        partial: false,
        not_in_context: false,
    }
}

#[test]
fn sessions_are_isolated_and_forgotten_on_close() {
    let store = ReadFileState::default();
    let (a, b) = (SessionId::new("a"), SessionId::new("b"));
    store.set(&a, "/w/x", state("x", "v1"));
    assert!(store.get(&b, "/w/x").is_none());
    store.fork(&a, &b);
    assert_eq!(store.get(&b, "/w/x").unwrap().content, "x");
    store.forget(&a);
    assert!(store.get(&a, "/w/x").is_none());
    assert!(store.get(&b, "/w/x").is_some());
}

#[test]
fn least_recently_used_entries_and_sessions_are_evicted() {
    let store = ReadFileState::default();
    let session = SessionId::new("s");
    for index in 0..=MAX_ENTRIES {
        store.set(&session, &format!("/w/{index}"), state("x", "v"));
    }
    assert!(store.get(&session, "/w/0").is_none());
    assert!(store.get(&session, &format!("/w/{MAX_ENTRIES}")).is_some());
    for index in 0..MAX_SESSIONS {
        store.set(
            &SessionId::new(format!("other{index}")),
            "/w/x",
            state("x", "v"),
        );
    }
    assert!(store.get(&session, "/w/1").is_none());
}

#[test]
fn changes_are_judged_by_version_then_by_content_for_full_views() {
    let full = state("same", "v1");
    assert!(!full.changed(&FileVersion::new("v1"), || None));
    assert!(!full.changed(&FileVersion::new("v2"), || Some("same".to_owned())));
    assert!(full.changed(&FileVersion::new("v2"), || Some("other".to_owned())));
    let ranged = FileState {
        offset: Some(5),
        ..state("same", "v1")
    };
    assert!(ranged.changed(&FileVersion::new("v2"), || Some("same".to_owned())));
    let restored = FileState {
        baseline: Baseline::Content,
        ..state("same", "v1")
    };
    assert!(!restored.changed(&FileVersion::new("any"), || Some("same".to_owned())));
    assert!(restored.changed(&FileVersion::new("any"), || Some("new".to_owned())));
}

#[test]
fn limited_reads_count_as_full_only_when_the_file_ended_early() {
    let short = FileState {
        limit: Some(10),
        ..state("a\nb", "v")
    };
    assert!(short.is_full_view());
    let cut = FileState {
        limit: Some(2),
        ..state("a\nb", "v")
    };
    assert!(!cut.is_full_view());
    let hidden = FileState {
        not_in_context: true,
        offset: None,
        ..state("a", "v")
    };
    assert!(hidden.is_full_view() && !hidden.is_in_context());
}

/// 压缩时的清空（compact.md §3.4，先取出再清空）：返回清空前的路径，最近用过的在前；别的会话不动。
#[test]
fn clearing_for_compaction_returns_the_paths_most_recent_first() {
    use rust_cli_kernel_api::tool::ReadFileStates;
    let store = ReadFileState::default();
    let (a, b) = (SessionId::new("a"), SessionId::new("b"));
    store.set(&a, "/w/old", state("o", "v"));
    store.set(&a, "/w/new", state("n", "v"));
    store.set(&b, "/w/other", state("x", "v"));
    // 读一次也算用过（LRU）。
    assert!(store.get(&a, "/w/old").is_some());
    assert_eq!(ReadFileStates::clear(&store, &a), ["/w/old", "/w/new"]);
    assert!(store.get(&a, "/w/old").is_none());
    assert!(store.get(&a, "/w/new").is_none());
    assert!(store.get(&b, "/w/other").is_some());
    assert!(ReadFileStates::clear(&store, &SessionId::new("none")).is_empty());
}
