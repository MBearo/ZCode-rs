//! 工作区信任的判定与回答的处理（docs/specs/rust-cli/workspace-trust.md §2.1、§2.5）：内存文件系统与内存存储，
//! 对话框用替身（记下收到的提示、给出预设的回答）。
use std::sync::Mutex;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::EnvironmentId;
use rust_cli_kernel_api::testing::{MemoryEnvironment, MemoryStore};
use rust_cli_kernel_api::workspace_trust::TrustKey;
use rust_cli_tui::trust::{Grants, TrustAnswer, TrustPrompt};

use super::*;

const HOME: &str = "/Users/dev";

fn env(files: &[(&str, &str)]) -> MemoryEnvironment {
    let facts = EnvFacts {
        cwd: EnvPath::new("/"),
        platform: "darwin".to_owned(),
        os_version: "Darwin".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: Some(EnvPath::new(HOME)),
    };
    files
        .iter()
        .fold(MemoryEnvironment::new(facts), |env, (path, text)| {
            env.with_file(path, text)
        })
}

fn workspace(root: &str) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root),
    }
}

fn grants() -> Grants {
    Grants {
        count: 1,
        rules: vec!["Bash".to_owned()],
        sources: vec!["zcode.json".to_owned()],
    }
}

/// 跑一次判定；`answer` 是对话框替身的回答。返回结果、对话框收到的提示与写下的记录。
async fn run(
    env: &MemoryEnvironment,
    store: &MemoryStore,
    root: &str,
    grants: Option<Grants>,
    answer: TrustAnswer,
) -> (Outcome, Option<TrustPrompt>, Vec<String>) {
    let asked = Mutex::new(None);
    let outcome = ensure(
        Inputs {
            env,
            store,
            workspace: &workspace(root),
            grants,
        },
        async |prompt| {
            *asked.lock().unwrap() = Some(prompt);
            Ok(answer)
        },
    )
    .await
    .unwrap();
    let keys = store
        .trusted_keys()
        .iter()
        .map(|key| key.as_str().to_owned())
        .collect();
    (outcome, asked.into_inner().unwrap(), keys)
}

async fn trust(store: &MemoryStore, key: &str) {
    store.trust_workspace(&TrustKey::new(key)).await.unwrap();
}

#[tokio::test]
async fn an_untrusted_folder_asks_and_records_the_answer_at_the_folder() {
    let (env, store) = (env(&[]), MemoryStore::default());
    let (outcome, asked, keys) = run(&env, &store, "/w/a/b", None, TrustAnswer::Trusted).await;
    assert_eq!(outcome, Outcome::Continue);
    let asked = asked.expect("the dialog was shown");
    assert_eq!(asked.path, "/w/a/b");
    assert!(!asked.backstop);
    assert_eq!(keys, ["/w/a/b"]);
    // 记过之后不再问。
    let (outcome, asked, _) = run(&env, &store, "/w/a/b", None, TrustAnswer::Exit(1)).await;
    assert_eq!((outcome, asked), (Outcome::Continue, None));
}

#[tokio::test]
async fn declining_exits_with_the_dialog_code_and_records_nothing() {
    let (env, store) = (env(&[]), MemoryStore::default());
    for code in [0, 1] {
        let (outcome, _, keys) = run(&env, &store, "/w", None, TrustAnswer::Exit(code)).await;
        assert_eq!(outcome, Outcome::Exit(code));
        assert!(keys.is_empty());
    }
}

#[tokio::test]
async fn inside_a_repository_the_record_goes_to_the_git_root() {
    let env = env(&[("/w/repo/.git/HEAD", "ref: refs/heads/main\n")]);
    let store = MemoryStore::default();
    let (_, asked, keys) = run(&env, &store, "/w/repo/a/b", None, TrustAnswer::Trusted).await;
    assert!(asked.is_some());
    assert_eq!(keys, ["/w/repo"]);
}

#[tokio::test]
async fn a_dot_git_that_is_neither_file_nor_directory_is_not_a_git_root() {
    // git 根只认目录与文件；FIFO 等别的种类继续往上（F58：以前任何种类都算）。
    let env = env(&[("/w/repo/.git/HEAD", "ref")]).with_special("/w/repo/a/.git");
    let store = MemoryStore::default();
    let (_, _, keys) = run(&env, &store, "/w/repo/a/b", None, TrustAnswer::Trusted).await;
    assert_eq!(keys, ["/w/repo"]);
}

#[tokio::test]
async fn a_parent_grant_counts_outside_repositories_only_up_to_the_git_root_inside() {
    // 不在仓库里：父目录的记录一直向上算。
    let (plain, store) = (env(&[]), MemoryStore::default());
    trust(&store, "/w").await;
    let (_, asked, _) = run(&plain, &store, "/w/a/b", None, TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
    // 在仓库里：仓库外的父目录不算，仓库内的中间目录算（§2.1）。
    let repo = env(&[("/w/repo/.git/HEAD", "ref")]);
    let (_, asked, _) = run(&repo, &store, "/w/repo/a/b", None, TrustAnswer::Exit(1)).await;
    assert!(asked.is_some());
    trust(&store, "/w/repo/a").await;
    let (outcome, asked, _) = run(&repo, &store, "/w/repo/a/b", None, TrustAnswer::Exit(1)).await;
    assert_eq!((outcome, asked), (Outcome::Continue, None));
}

#[tokio::test]
async fn a_worktree_shares_the_trust_of_its_main_repository() {
    let env = env(&[
        ("/w/main/.git/HEAD", "ref"),
        ("/w/main/.git/worktrees/wt/commondir", "../..\n"),
        ("/w/main/.git/worktrees/wt/gitdir", "/w/trees/wt/.git\n"),
        ("/w/trees/wt/.git", "gitdir: /w/main/.git/worktrees/wt\n"),
    ]);
    let store = MemoryStore::default();
    // 在 worktree 里同意：记录写在主仓库根（项目路径是规范的 git 根）。
    let (_, asked, keys) = run(&env, &store, "/w/trees/wt/src", None, TrustAnswer::Trusted).await;
    assert!(asked.is_some());
    assert_eq!(keys, ["/w/main"]);
    // 主仓库根信任过：别的 worktree 与主仓库都不再问。
    let (_, asked, _) = run(&env, &store, "/w/trees/wt", None, TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
    let (_, asked, _) = run(&env, &store, "/w/main/x", None, TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
}

#[tokio::test]
async fn a_git_file_that_does_not_point_back_is_its_own_root() {
    // `gitdir` 回指别处（或 commondir 不对）时规范根就是 git 根。
    let env = env(&[
        ("/w/main/.git/worktrees/wt/commondir", "../..\n"),
        ("/w/main/.git/worktrees/wt/gitdir", "/elsewhere/.git\n"),
        ("/w/trees/wt/.git", "gitdir: /w/main/.git/worktrees/wt\n"),
    ]);
    let store = MemoryStore::default();
    let (_, _, keys) = run(&env, &store, "/w/trees/wt", None, TrustAnswer::Trusted).await;
    assert_eq!(keys, ["/w/trees/wt"]);
}

#[tokio::test]
async fn the_home_folder_is_trusted_for_this_run_only() {
    let (env, store) = (env(&[]), MemoryStore::default());
    let (outcome, asked, keys) = run(&env, &store, HOME, None, TrustAnswer::Trusted).await;
    assert_eq!(outcome, Outcome::Continue);
    assert!(asked.is_some());
    assert!(keys.is_empty(), "{keys:?}");
    // 下一次还问；home 有（手工写的）记录时不问。
    let (_, asked, _) = run(&env, &store, HOME, None, TrustAnswer::Exit(1)).await;
    assert!(asked.is_some());
    trust(&store, HOME).await;
    let (_, asked, _) = run(&env, &store, HOME, None, TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
    // home 下不在仓库里的子目录随 home 的记录。
    let (_, asked, _) = run(&env, &store, "/Users/dev/p", None, TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
}

#[tokio::test]
async fn a_parent_grant_with_project_grants_asks_again_as_a_backstop() {
    let (env, store) = (env(&[]), MemoryStore::default());
    trust(&store, "/w").await;
    // 没有项目授权：经父目录信任就够了。
    let (_, asked, _) = run(&env, &store, "/w/a", None, TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
    // 有项目授权：后备询问；拒绝时不带授权继续、不写记录。
    let (outcome, asked, keys) = run(
        &env,
        &store,
        "/w/a",
        Some(grants()),
        TrustAnswer::WithoutGrants,
    )
    .await;
    assert_eq!(outcome, Outcome::ContinueWithoutGrants);
    let asked = asked.unwrap();
    assert!(asked.backstop);
    assert_eq!(asked.grants, Some(grants()));
    assert_eq!(keys, ["/w"]);
    // 同意时写项目键，之后直接信任、不再问。
    let (outcome, _, keys) = run(&env, &store, "/w/a", Some(grants()), TrustAnswer::Trusted).await;
    assert_eq!(outcome, Outcome::Continue);
    assert_eq!(keys, ["/w", "/w/a"]);
    let (_, asked, _) = run(&env, &store, "/w/a", Some(grants()), TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
    // 未信任时项目授权只出现在提示里，不是后备询问。
    let (_, asked, _) = run(&env, &store, "/x", Some(grants()), TrustAnswer::Exit(1)).await;
    let asked = asked.unwrap();
    assert!(!asked.backstop);
    assert_eq!(asked.grants, Some(grants()));
    // home 不做后备询问（排除的目录含 home）。
    trust(&store, "/Users").await;
    let (_, asked, _) = run(&env, &store, HOME, Some(grants()), TrustAnswer::Exit(1)).await;
    assert_eq!(asked, None);
}
