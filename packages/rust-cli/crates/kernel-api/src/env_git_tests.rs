use serde_json::json;

use super::*;

#[test]
fn git_status_round_trips_with_tagged_variants() {
    let snapshot = GitStatus::Repository(GitSnapshot {
        branch: "main".to_owned(),
        default_branch: "main".to_owned(),
        status: "?? new.txt".to_owned(),
        recent_commits: "abc1234 first".to_owned(),
        user_name: Some("dev".to_owned()),
    });
    let value = serde_json::to_value(&snapshot).unwrap();
    assert_eq!(value["kind"], "repository");
    assert_eq!(value["user_name"], "dev");
    assert_eq!(
        serde_json::from_value::<GitStatus>(value).unwrap(),
        snapshot
    );
    let none = serde_json::to_value(GitStatus::NotRepository).unwrap();
    assert_eq!(none, json!({"kind": "not_repository"}));
}

#[test]
fn missing_user_name_is_omitted_and_defaults_to_none() {
    let snapshot = GitSnapshot {
        branch: "HEAD".to_owned(),
        default_branch: "main".to_owned(),
        status: String::new(),
        recent_commits: String::new(),
        user_name: None,
    };
    let value = serde_json::to_value(&snapshot).unwrap();
    assert!(value.get("user_name").is_none());
    assert_eq!(
        serde_json::from_value::<GitSnapshot>(value).unwrap(),
        snapshot
    );
    let request = GitStatusRequest {
        cwd: EnvPath::new("/Users/dev/work"),
    };
    assert_eq!(
        serde_json::to_value(&request).unwrap(),
        json!({"cwd": "/Users/dev/work"})
    );
}

#[test]
fn diffs_round_trip() {
    let diff = GitDiff::Repository {
        numstat: "1\t0\ta.rs".to_owned(),
        patch: "diff --git a/a.rs b/a.rs".to_owned(),
        untracked: "new.txt".to_owned(),
    };
    let json = serde_json::to_value(&diff).unwrap();
    assert_eq!(json["kind"], "repository");
    assert_eq!(serde_json::from_value::<GitDiff>(json).unwrap(), diff);
    let request = GitDiffRequest {
        cwd: EnvPath::new("/Users/dev/project"),
    };
    let json = serde_json::to_value(&request).unwrap();
    assert_eq!(
        serde_json::from_value::<GitDiffRequest>(json).unwrap(),
        request
    );
}

#[test]
fn only_files_and_directories_mark_a_git_root() {
    // git 根的标记：`isDirectory() || isFile()`；FIFO、socket、设备（`Other`）不算（F58）。
    assert!(marks_git_root(FileKind::Directory));
    assert!(marks_git_root(FileKind::File));
    assert!(!marks_git_root(FileKind::Other));
}

#[test]
fn the_gitdir_pointer_is_trimmed_and_unc_is_refused() {
    assert_eq!(
        gitdir_pointer("  gitdir:  ../main/.git/worktrees/a \n"),
        Some("../main/.git/worktrees/a")
    );
    assert_eq!(gitdir_pointer("gitdir:/w/.git"), Some("/w/.git"));
    assert_eq!(gitdir_pointer("ref: refs/heads/main"), None);
    // 网络路径不读。
    assert_eq!(gitdir_pointer("gitdir: //server/share/.git"), None);
    assert_eq!(gitdir_pointer(r"gitdir: \\server\share\.git"), None);
    assert_eq!(gitdir_pointer(r"gitdir: C:\w\.git"), Some(r"C:\w\.git"));
}
