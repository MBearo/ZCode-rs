#![expect(
    clippy::disallowed_methods,
    reason = "测试直接准备临时 git 仓库并读取 PATH"
)]

use std::path::Path;

use rust_cli_kernel_api::env::{EnvPath, Environment, WorkspaceRef};
use rust_cli_kernel_api::env_git::{
    GitDiff, GitDiffRequest, GitSnapshot, GitStatus, GitStatusRequest,
};
use rust_cli_kernel_api::ids::EnvironmentId;

use crate::{LocalEnvironment, ShellConfig};

/// 测试用的进程环境：真实 PATH（找到 git），HOME / XDG 指向临时目录，不读本机的 git 全局配置。
fn snapshot(home: &Path) -> Vec<(String, String)> {
    let home = home.to_string_lossy().into_owned();
    vec![
        ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
        ("HOME".to_owned(), home.clone()),
        ("XDG_CONFIG_HOME".to_owned(), home),
    ]
}

fn local(home: &Path) -> LocalEnvironment {
    LocalEnvironment::new(Some(home.to_path_buf())).with_shell(ShellConfig {
        env: snapshot(home).into(),
        ..ShellConfig::default()
    })
}

/// 在 `dir` 里跑 git（固定作者与时间，提交哈希确定）。
pub(crate) fn git(dir: &Path, home: &Path, args: &[&str]) {
    let status = rust_cli_proc::std_command("git")
        .args(args)
        .current_dir(dir)
        .env_clear()
        .envs(snapshot(home))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Dev")
        .env("GIT_AUTHOR_EMAIL", "dev@example.com")
        .env("GIT_COMMITTER_NAME", "Dev")
        .env("GIT_COMMITTER_EMAIL", "dev@example.com")
        .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .status()
        .unwrap();
    assert!(status.success(), "git {args:?} failed");
}

fn workspace(root: &Path) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root.to_string_lossy()),
    }
}

async fn status(env: &LocalEnvironment, cwd: &Path) -> GitStatus {
    env.git_status(
        &workspace(cwd),
        GitStatusRequest {
            cwd: EnvPath::new(cwd.to_string_lossy()),
        },
    )
    .await
    .unwrap()
}

fn snapshot_of(status: GitStatus) -> GitSnapshot {
    match status {
        GitStatus::Repository(snapshot) => snapshot,
        GitStatus::NotRepository => panic!("expected a repository"),
    }
}

#[tokio::test]
async fn a_directory_without_git_is_not_a_repository() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    assert_eq!(
        status(&local(home.path()), work.path()).await,
        GitStatus::NotRepository
    );
}

#[tokio::test]
async fn snapshot_has_branch_status_commits_and_user() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    git(root, home.path(), &["init", "-q", "-b", "main"]);
    git(root, home.path(), &["config", "user.name", "Dev Example"]);
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    git(root, home.path(), &["add", "a.txt"]);
    git(root, home.path(), &["commit", "-q", "-m", "first"]);
    std::fs::write(root.join("a.txt"), "two\n").unwrap();
    git(root, home.path(), &["commit", "-q", "-am", "second"]);
    std::fs::write(root.join("new.txt"), "x\n").unwrap();
    std::fs::create_dir(root.join("sub")).unwrap();

    // 从子目录问也找到同一个仓库（向上找 `.git`）；git 在 cwd 里跑，
    // 所以路径相对 cwd。
    let found = snapshot_of(status(&local(home.path()), &root.join("sub")).await);
    assert_eq!(found.branch, "main");
    assert_eq!(found.default_branch, "main");
    assert_eq!(found.status, "?? ../new.txt");
    let commits: Vec<&str> = found.recent_commits.lines().collect();
    assert_eq!(commits.len(), 2);
    assert!(commits[0].ends_with(" second"), "{commits:?}");
    assert!(commits[1].ends_with(" first"), "{commits:?}");
    assert_eq!(found.user_name.as_deref(), Some("Dev Example"));
}

#[tokio::test]
async fn unborn_and_detached_heads_are_reported() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    git(root, home.path(), &["init", "-q", "-b", "trunk"]);
    // 还没有提交：分支名照常给出（读 HEAD 文件），log 失败时为空，没有 user.name。
    let unborn = snapshot_of(status(&local(home.path()), root).await);
    assert_eq!(unborn.branch, "trunk");
    assert_eq!(unborn.recent_commits, "");
    assert_eq!(unborn.status, "");
    assert_eq!(unborn.user_name, None);
    assert_eq!(unborn.default_branch, "main");

    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    git(root, home.path(), &["add", "a.txt"]);
    git(root, home.path(), &["commit", "-q", "-m", "first"]);
    git(root, home.path(), &["checkout", "-q", "--detach"]);
    let detached = snapshot_of(status(&local(home.path()), root).await);
    assert_eq!(detached.branch, "HEAD");
}

#[tokio::test]
async fn default_branch_prefers_origin_head_then_main_then_master() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    git(root, home.path(), &["init", "-q", "-b", "feature"]);
    std::fs::write(root.join("a.txt"), "one\n").unwrap();
    git(root, home.path(), &["add", "a.txt"]);
    git(root, home.path(), &["commit", "-q", "-m", "first"]);
    let env = local(home.path());

    git(
        root,
        home.path(),
        &["update-ref", "refs/remotes/origin/master", "HEAD"],
    );
    assert_eq!(
        snapshot_of(status(&env, root).await).default_branch,
        "master"
    );

    git(
        root,
        home.path(),
        &["update-ref", "refs/remotes/origin/develop", "HEAD"],
    );
    let head = "refs/remotes/origin/HEAD";
    git(
        root,
        home.path(),
        &["symbolic-ref", head, "refs/remotes/origin/develop"],
    );
    assert_eq!(
        snapshot_of(status(&env, root).await).default_branch,
        "develop"
    );

    // origin/HEAD 指向不存在的分支时不算，退回 main / master。
    git(
        root,
        home.path(),
        &["symbolic-ref", head, "refs/remotes/origin/gone"],
    );
    git(
        root,
        home.path(),
        &["update-ref", "refs/remotes/origin/main", "HEAD"],
    );
    assert_eq!(snapshot_of(status(&env, root).await).default_branch, "main");
}

/// `/diff` 的未提交改动（tui-manage.md §4.1）：修改、二进制与未跟踪文件；仓库外是 NotRepository。
#[tokio::test]
async fn uncommitted_changes_come_back_as_raw_git_output() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let root = work.path();
    let env = local(home.path());
    let request = |cwd: &Path| GitDiffRequest {
        cwd: EnvPath::new(cwd.to_string_lossy()),
    };
    assert_eq!(
        env.git_diff(&workspace(root), request(root)).await.unwrap(),
        GitDiff::NotRepository
    );
    git(root, home.path(), &["init", "-q", "-b", "main"]);
    std::fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    std::fs::write(root.join("b.bin"), [0u8, 1, 2]).unwrap();
    git(root, home.path(), &["add", "."]);
    git(root, home.path(), &["commit", "-q", "-m", "first"]);
    std::fs::write(root.join("a.txt"), "one\n2\nthree\n").unwrap();
    std::fs::write(root.join("b.bin"), [3u8, 0, 4]).unwrap();
    std::fs::write(root.join("new.txt"), "x\n").unwrap();
    let GitDiff::Repository {
        numstat,
        patch,
        untracked,
    } = env.git_diff(&workspace(root), request(root)).await.unwrap()
    else {
        panic!("not a repository");
    };
    assert_eq!(numstat, "2\t1\ta.txt\n-\t-\tb.bin");
    assert!(patch.starts_with("diff --git a/a.txt b/a.txt"), "{patch}");
    assert!(patch.contains("\n-two\n+2\n+three"), "{patch}");
    assert_eq!(untracked, "new.txt");
}
