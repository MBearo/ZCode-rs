use std::path::Path;

use rust_cli_paths::{StoragePaths, StorageSettings};

use super::*;

fn paths(home: &Path) -> StoragePaths {
    StoragePaths::resolve(&StorageSettings::default(), home, home)
}

#[tokio::test]
async fn entries_are_appended_in_order_and_loaded_per_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let (recorder, task) = History::new(&paths).writer("/a".to_owned());
    recorder.record("first", 1);
    recorder.record("second", 2);
    drop(recorder);
    task.await;
    let (other, task) = History::new(&paths).writer("/b".to_owned());
    other.record("elsewhere", 3);
    drop(other);
    task.await;
    let history = History::new(&paths);
    assert_eq!(history.load("/a").await, ["first", "second"]);
    assert_eq!(history.load("/b").await, ["elsewhere"]);
    // Host TUI 的行形状。
    #[expect(clippy::disallowed_methods, reason = "测试读回历史文件")]
    let text = std::fs::read_to_string(paths.tui_history().as_path()).unwrap();
    assert!(
        text.starts_with(r#"{"workspace":"/a","text":"first","at":1}"#),
        "{text}"
    );
}

#[tokio::test]
async fn only_the_last_entries_are_loaded_and_bad_lines_are_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let paths = paths(dir.path());
    let mut text = String::from("not json\n");
    for index in 0..(LOAD + 5) {
        text.push_str(&format!(
            "{{\"workspace\":\"/a\",\"text\":\"t{index}\",\"at\":{index}}}\n"
        ));
    }
    rust_cli_fs::append(paths.tui_history().as_path(), text.as_bytes())
        .await
        .unwrap();
    let loaded = History::new(&paths).load("/a").await;
    assert_eq!(loaded.len(), LOAD);
    assert_eq!(loaded.first().map(String::as_str), Some("t5"));
    assert!(History::new(&paths).load("/missing").await.is_empty());
}
