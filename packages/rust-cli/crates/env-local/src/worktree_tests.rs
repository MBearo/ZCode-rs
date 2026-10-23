#![expect(
    clippy::disallowed_methods,
    reason = "测试直接准备临时 git 仓库并读取 PATH"
)]

use std::collections::BTreeMap;
use std::path::Path;

use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_worktree::{
    WorktreeCreate, WorktreeCreated, WorktreeInspect, WorktreeRemove,
};

use super::*;
use crate::git::tests::git;

fn projected(home: &Path) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
        ("HOME".to_owned(), home.to_string_lossy().into_owned()),
        (
            "XDG_CONFIG_HOME".to_owned(),
            home.to_string_lossy().into_owned(),
        ),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
        ("GIT_AUTHOR_NAME".to_owned(), "Dev".to_owned()),
        ("GIT_AUTHOR_EMAIL".to_owned(), "dev@example.com".to_owned()),
        ("GIT_COMMITTER_NAME".to_owned(), "Dev".to_owned()),
        (
            "GIT_COMMITTER_EMAIL".to_owned(),
            "dev@example.com".to_owned(),
        ),
    ])
}

fn repo(home: &Path, dir: &Path) {
    git(dir, home, &["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    git(dir, home, &["add", "."]);
    git(dir, home, &["commit", "-q", "-m", "init"]);
}

#[tokio::test]
async fn creates_resumes_lists_inspects_and_removes_a_worktree() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().canonicalize().unwrap();
    repo(home.path(), &root);
    let env = || projected(home.path());
    let request = WorktreeCreate {
        cwd: EnvPath::new(root.to_string_lossy()),
        name: "user/feature".to_owned(),
    };
    // 没有 origin：fetch 失败，退回 HEAD。
    let WorktreeCreated::Ready {
        path,
        branch,
        head_commit,
        existed,
    } = create(request.clone(), env()).await.unwrap()
    else {
        panic!("not created");
    };
    let expected = root.join(".zcode/worktrees/user+feature");
    assert_eq!(path.as_str(), expected.to_string_lossy());
    assert_eq!(branch, "worktree-user+feature");
    assert!(!existed);
    assert!(expected.join("a.txt").is_file());
    // 同名再来一次：原样续用。
    let again = create(request, env()).await.unwrap();
    assert!(matches!(
        again,
        WorktreeCreated::Ready { existed: true, .. }
    ));

    let listed = list(&EnvPath::new(expected.to_string_lossy()), env())
        .await
        .unwrap();
    assert_eq!(
        listed.root.as_ref().map(EnvPath::as_str),
        Some(&*root.to_string_lossy())
    );
    assert!(listed.entries.iter().any(
        |entry| entry.path == path && entry.branch.as_deref() == Some("worktree-user+feature")
    ));

    // 一个未提交文件、一个新提交。
    std::fs::write(expected.join("b.txt"), "b\n").unwrap();
    std::fs::write(expected.join("a.txt"), "changed\n").unwrap();
    git(&expected, home.path(), &["commit", "-q", "-am", "work"]);
    let found = changes(
        WorktreeInspect {
            path: path.clone(),
            base: head_commit,
        },
        env(),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!((found.changed_files, found.commits), (1, 1));

    let removed = remove(
        WorktreeRemove {
            path,
            branch: Some(branch),
        },
        env(),
    )
    .await
    .unwrap();
    assert!(removed);
    assert!(!expected.exists());
}

#[tokio::test]
async fn outside_a_repository() {
    let home = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();
    let request = WorktreeCreate {
        cwd: EnvPath::new(dir.path().to_string_lossy()),
        name: "a".to_owned(),
    };
    assert_eq!(
        create(request, projected(home.path())).await.unwrap(),
        WorktreeCreated::NotARepository
    );
    let listed = list(
        &EnvPath::new(dir.path().to_string_lossy()),
        projected(home.path()),
    )
    .await
    .unwrap();
    assert!(listed.root.is_none() && listed.entries.is_empty());
}
