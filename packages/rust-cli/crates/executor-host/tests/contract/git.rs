//! `Git` 与 `Files::ensure_dir`（F110，docs/specs/rust-cli/deployment.md §19.1）：git 跟着会话的主 env，经执行器时与
//! 本机的结果相同。夹具是临时目录里的真实 git 仓库（`git` 经 PATH 找，环境只给测试用的身份）。

use std::path::{Path, PathBuf};
use std::process::Command;

use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_kernel_api::env_files::EnsureDir;
use rust_cli_kernel_api::env_git::{GitDiff, GitDiffRequest, GitStatus, GitStatusRequest};

use crate::contract;
use crate::support::{Kind, Machine, machine, workspace};

struct Fixture {
    _dir: tempfile::TempDir,
    base: PathBuf,
    repo: PathBuf,
    machine: Machine,
}

fn git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_AUTHOR_NAME", "dev")
        .env("GIT_AUTHOR_EMAIL", "dev@example.com")
        .env("GIT_COMMITTER_NAME", "dev")
        .env("GIT_COMMITTER_EMAIL", "dev@example.com")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?}");
}

async fn fixture(kind: Kind) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let base = dir.path().canonicalize().unwrap();
    let repo = base.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.name", "dev"]);
    std::fs::write(repo.join("a.txt"), "one\n").unwrap();
    git(&repo, &["add", "a.txt"]);
    git(&repo, &["commit", "-q", "-m", "first"]);
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(repo.join("new.txt"), "new\n").unwrap();
    let local = LocalEnvironment::new(Some(base.clone())).with_shell(ShellConfig {
        scratch: base.join("scratch"),
        ledger: base.join("ledger"),
        bash: None,
        bwrap: None,
        embedded: None,
        env: vec![(
            "PATH".to_owned(),
            "/usr/bin:/bin:/usr/local/bin:/opt/homebrew/bin".to_owned(),
        )]
        .into(),
    });
    Fixture {
        _dir: dir,
        base,
        repo,
        machine: machine(kind, local).await,
    }
}

fn env_path(path: &Path) -> EnvPath {
    EnvPath::new(path.to_string_lossy())
}

async fn git_status_reports_the_repository(kind: Kind) {
    let fixture = fixture(kind).await;
    let ws = workspace(&fixture.repo);
    let request = GitStatusRequest {
        cwd: env_path(&fixture.repo),
    };
    let status = fixture
        .machine
        .git()
        .git_status(&ws, request)
        .await
        .unwrap();
    let GitStatus::Repository(snapshot) = status else {
        panic!("not a repository: {status:?}");
    };
    assert_eq!(snapshot.branch, "main");
    assert!(snapshot.status.contains("a.txt"), "{snapshot:?}");
    assert!(snapshot.status.contains("new.txt"), "{snapshot:?}");
    assert!(snapshot.recent_commits.contains("first"), "{snapshot:?}");
    assert_eq!(snapshot.user_name.as_deref(), Some("dev"));
}

async fn git_status_outside_a_repository(kind: Kind) {
    let fixture = fixture(kind).await;
    let plain = fixture.base.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let request = GitStatusRequest {
        cwd: env_path(&plain),
    };
    let status = fixture
        .machine
        .git()
        .git_status(&workspace(&plain), request)
        .await
        .unwrap();
    assert_eq!(status, GitStatus::NotRepository);
}

async fn git_diff_lists_the_uncommitted_changes(kind: Kind) {
    let fixture = fixture(kind).await;
    let request = GitDiffRequest {
        cwd: env_path(&fixture.repo),
    };
    let diff = fixture
        .machine
        .git()
        .git_diff(&workspace(&fixture.repo), request)
        .await
        .unwrap();
    let GitDiff::Repository {
        numstat,
        patch,
        untracked,
    } = diff
    else {
        panic!("not a repository: {diff:?}");
    };
    assert!(numstat.contains("a.txt"), "{numstat}");
    assert!(patch.contains("+two"), "{patch}");
    assert!(untracked.contains("new.txt"), "{untracked}");
}

async fn worktree_list_names_the_main_worktree(kind: Kind) {
    let fixture = fixture(kind).await;
    let list = fixture
        .machine
        .git()
        .worktree_list(env_path(&fixture.repo))
        .await
        .unwrap();
    assert_eq!(list.root, Some(env_path(&fixture.repo)));
    assert!(
        list.entries
            .iter()
            .any(|entry| entry.path == env_path(&fixture.repo)
                && entry.branch.as_deref() == Some("main")),
        "{list:?}"
    );
}

async fn ensure_dir_creates_the_parents_and_is_idempotent(kind: Kind) {
    let fixture = fixture(kind).await;
    let ws = workspace(&fixture.base);
    let dir = fixture.base.join("home/.zcode/projects/-repo/memory");
    for _ in 0..2 {
        let request = EnsureDir {
            path: env_path(&dir),
        };
        fixture
            .machine
            .files()
            .ensure_dir(&ws, request)
            .await
            .unwrap();
        assert!(dir.is_dir());
    }
    // 路径上是一个文件时报错，不覆盖它。
    let file = fixture.base.join("taken");
    std::fs::write(&file, "x").unwrap();
    let request = EnsureDir {
        path: env_path(&file),
    };
    assert!(
        fixture
            .machine
            .files()
            .ensure_dir(&ws, request)
            .await
            .is_err()
    );
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "x");
}

contract!(
    git_status_reports_the_repository,
    git_status_outside_a_repository,
    git_diff_lists_the_uncommitted_changes,
    worktree_list_names_the_main_worktree,
    ensure_dir_creates_the_parents_and_is_idempotent,
);
