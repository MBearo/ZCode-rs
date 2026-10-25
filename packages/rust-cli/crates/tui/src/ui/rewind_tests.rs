use rust_cli_kernel_api::session_api::FileChange;

use super::*;

fn change(path: &str, additions: u64, deletions: u64) -> FileChange {
    FileChange {
        path: path.into(),
        additions,
        deletions,
        machine: None,
    }
}

/// 列表第二行与确认页的文字照选择器的原文。
#[test]
fn summaries_follow_the_message_selector_text() {
    assert_eq!(summary(&[]), "No code changes");
    assert_eq!(summary(&[change("/w/src/a.rs", 3, 1)]), "a.rs +3 -1");
    assert_eq!(
        summary(&[change("/w/a", 1, 0), change("/w/b", 2, 2)]),
        "2 files changed +3 -2"
    );
    assert_eq!(
        code_text(&[change("/w/a.rs", 1, 2)]),
        "The code will be restored +1 -2 in a.rs."
    );
    assert_eq!(
        code_text(&[
            change("/w/a", 1, 0),
            change("/w/b", 0, 1),
            change("/w/c", 1, 1)
        ]),
        "The code will be restored +2 -2 in a and 2 other files."
    );
    assert_eq!(
        code_text(&[]),
        "The code has not changed (nothing will be restored)."
    );
}

/// F98（remote-exec.md §25.4.5，Docker 试用 L8）：远端的文件写 `dev-a:README.md`；两台机器上的同名文件分开计。旧代码只写
/// 文件名，同名文件合在一起。
#[test]
fn remote_files_name_their_machine() {
    let remote = |path: &str, additions, deletions| FileChange {
        machine: Some("dev-a".into()),
        ..change(path, additions, deletions)
    };
    assert_eq!(
        summary(&[remote("/srv/proj/README.md", 1, 1)]),
        "dev-a:README.md +1 -1"
    );
    assert_eq!(
        code_text(&[remote("/srv/proj/README.md", 1, 1)]),
        "The code will be restored +1 -1 in dev-a:README.md."
    );
    let entry = |files: Vec<FileChange>, message: &str| Entry {
        option: rust_cli_kernel_api::session_api::RewindOption {
            message: MessageId::new(message),
            files,
            code: true,
        },
        text: message.into(),
    };
    let entries = [
        entry(vec![change("/w/same.txt", 1, 0)], "m1"),
        entry(vec![remote("/w/same.txt", 2, 0)], "m2"),
    ];
    let merged = changes_from(&entries, &MessageId::new("m1"));
    assert_eq!(merged.len(), 2, "{merged:?}");
    assert_eq!(
        code_text(&merged),
        "The code will be restored +3 -0 in same.txt and dev-a:same.txt."
    );
}
