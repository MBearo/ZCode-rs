use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::MemoryEnvironment;

use super::project_root;

fn facts() -> EnvFacts {
    EnvFacts {
        cwd: EnvPath::new("/Users/dev/repo/src"),
        platform: "darwin".to_owned(),
        os_version: "Darwin".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: true,
        additional_directories: Vec::new(),
        home: None,
    }
}

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

#[tokio::test]
async fn the_nearest_git_directory_wins_otherwise_the_workspace_root() {
    let env = MemoryEnvironment::new(facts()).with_file("/Users/dev/repo/.git/HEAD", "ref");
    let root = project_root(&env, &workspace("/Users/dev/repo/src")).await;
    assert_eq!(root.as_str(), "/Users/dev/repo");
    let plain = MemoryEnvironment::new(facts());
    let root = project_root(&plain, &workspace("/Users/dev/other")).await;
    assert_eq!(root.as_str(), "/Users/dev/other");
}

#[tokio::test]
async fn the_walk_reaches_the_root_in_both_styles() {
    let env = MemoryEnvironment::new(facts()).with_file("/.git/HEAD", "ref");
    let root = project_root(&env, &workspace("/Users/dev/repo/")).await;
    assert_eq!(root.as_str(), "/");
    let env = MemoryEnvironment::new(facts()).with_file(r"C:\.git\HEAD", "ref");
    let root = project_root(&env, &workspace(r"C:\work\repo")).await;
    assert_eq!(root.as_str(), r"C:\");
}

#[tokio::test]
async fn a_dot_git_that_is_neither_file_nor_directory_is_not_a_root() {
    // git 根只认目录与文件的 `.git`；FIFO 等别的种类继续往上（F58：以前任何种类都算）。
    let env = MemoryEnvironment::new(facts())
        .with_special("/Users/dev/repo/src/.git")
        .with_file("/Users/dev/repo/.git/HEAD", "ref");
    let root = project_root(&env, &workspace("/Users/dev/repo/src")).await;
    assert_eq!(root.as_str(), "/Users/dev/repo");
}
