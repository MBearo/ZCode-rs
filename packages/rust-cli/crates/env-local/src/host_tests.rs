#![expect(clippy::disallowed_methods, reason = "测试直接准备工作区夹具")]

use std::path::Path;

use rust_cli_kernel_api::env::{EnvPath, Environment, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;

use super::*;
use crate::git::tests::git;
use crate::{LocalEnvironment, ShellConfig};

fn workspace(root: &Path) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root.to_string_lossy()),
    }
}

#[test]
fn shell_is_described_by_its_name() {
    assert_eq!(describe_shell(Some("/bin/zsh"), true), "zsh");
    assert_eq!(describe_shell(Some("/usr/local/bin/bash"), true), "bash");
    assert_eq!(describe_shell(Some("/usr/bin/fish"), true), "/usr/bin/fish");
    assert_eq!(describe_shell(None, true), "unknown");
    if cfg!(windows) {
        assert_eq!(describe_shell(None, false), "PowerShell");
    } else {
        assert_eq!(describe_shell(None, false), "unknown");
    }
}

#[test]
fn platform_uses_node_names() {
    let expected = if cfg!(target_os = "macos") {
        "darwin"
    } else if cfg!(windows) {
        "win32"
    } else {
        std::env::consts::OS
    };
    assert_eq!(platform(), expected);
}

#[test]
fn git_root_is_the_nearest_ancestor_with_dot_git() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("repo/a/b")).unwrap();
    assert_eq!(git_root(&root.join("repo/a/b")), None);
    std::fs::create_dir(root.join("repo/.git")).unwrap();
    assert_eq!(git_root(&root.join("repo/a/b")), Some(root.join("repo")));
    // worktree 的 `.git` 是文件，也算。
    std::fs::write(root.join("repo/a/.git"), "gitdir: elsewhere\n").unwrap();
    assert_eq!(git_root(&root.join("repo/a/b")), Some(root.join("repo/a")));
}

/// 本机判定与经端口的判定是同一条规则（kernel-api `env_git::marks_git_root`，F58）：`.git` 是 socket 时
/// `stat` 给 `Other`，两边都不算 git 根、继续往上。
#[cfg(unix)]
#[tokio::test]
async fn a_dot_git_that_is_neither_file_nor_directory_is_not_a_git_root() {
    use rust_cli_kernel_api::env_files::{FileKind, StatFile};
    use rust_cli_kernel_api::env_git::has_git_marker;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("repo/a/b")).unwrap();
    std::fs::create_dir(root.join("repo/.git")).unwrap();
    let socket = root.join("repo/a/.git");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    assert_eq!(git_root(&root.join("repo/a/b")), Some(root.join("repo")));
    let env = LocalEnvironment::new(None);
    let ws = workspace(root);
    let path = EnvPath::new(socket.to_string_lossy());
    let stat = env.stat(&ws, StatFile { path }).await.unwrap().unwrap();
    assert_eq!(stat.kind, FileKind::Other);
    assert!(!has_git_marker(&env, &ws, &socket.to_string_lossy()).await);
    let directory = root.join("repo/.git").to_string_lossy().into_owned();
    assert!(has_git_marker(&env, &ws, &directory).await);
}

#[tokio::test]
async fn facts_come_from_the_local_host() {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let env = LocalEnvironment::new(Some(home.path().to_path_buf())).with_shell(ShellConfig {
        env: vec![
            ("SHELL".to_owned(), "/bin/zsh".to_owned()),
            ("PATH".to_owned(), std::env::var("PATH").unwrap_or_default()),
        ]
        .into(),
        bash: Some("/bin/bash".into()),
        ..ShellConfig::default()
    });
    let facts = env.facts(&workspace(work.path())).await.unwrap();
    assert_eq!(facts.cwd.as_str(), work.path().to_string_lossy());
    assert_eq!(facts.platform, platform());
    assert_eq!(facts.shell, "zsh");
    assert!(!facts.is_git_repo);
    assert_eq!(
        facts.home.as_ref().map(EnvPath::as_str),
        Some(&*home.path().to_string_lossy())
    );
    if cfg!(unix) {
        // Node 的 `${os.type()} ${os.release()}`，例如 `Darwin 24.6.0`、`Linux 6.8.0`。
        let kernel = if cfg!(target_os = "macos") {
            "Darwin "
        } else {
            "Linux "
        };
        assert!(facts.os_version.starts_with(kernel), "{}", facts.os_version);
    }

    git(work.path(), home.path(), &["init", "-q", "-b", "main"]);
    assert!(
        env.facts(&workspace(work.path()))
            .await
            .unwrap()
            .is_git_repo
    );
}

#[tokio::test]
async fn facts_without_a_shell_config_still_answer() {
    let work = tempfile::tempdir().unwrap();
    let facts = LocalEnvironment::new(None)
        .facts(&workspace(work.path()))
        .await
        .unwrap();
    assert_eq!(facts.shell, describe_shell(None, false));
    assert_eq!(facts.home, None);
}
