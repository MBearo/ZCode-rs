use rust_cli_kernel_api::session_api::FileChange;

use super::*;

fn change(path: &str, additions: u64, deletions: u64) -> FileChange {
    FileChange {
        path: path.into(),
        additions,
        deletions,
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
