//! 读过的文件在磁盘上被改动（tools-files.md §4.1）：查哪些记录、片段、去重、上限与敏感文件。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath, FileVersion, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::testing::MemoryEnvironment;
use rust_cli_kernel_api::tool::{ChangedFile, ChangedFiles};

use super::FileChanges;
use crate::file_state::{Baseline, FileState, ReadFileState};

fn workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/w"),
    }
}

fn env() -> Arc<MemoryEnvironment> {
    Arc::new(MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new("/w"),
        platform: "darwin".to_owned(),
        os_version: "Darwin 24".to_owned(),
        shell: "zsh".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: None,
    }))
}

/// Edit / Write 写下的形状：全文、没有 offset 与 limit。
fn written(content: &str) -> FileState {
    FileState {
        content: content.to_owned(),
        baseline: Baseline::Version(FileVersion::new("before")),
        offset: None,
        limit: None,
        partial: false,
        not_in_context: false,
    }
}

struct Case {
    env: Arc<MemoryEnvironment>,
    state: Arc<ReadFileState>,
    session: SessionId,
}

impl Case {
    fn new() -> Self {
        Self {
            env: env(),
            state: Arc::new(ReadFileState::default()),
            session: SessionId::new("s"),
        }
    }

    async fn check(&self) -> Vec<ChangedFile> {
        FileChanges::new(self.state.clone(), self.env.clone())
            .changed_files(&self.session, &workspace())
            .await
    }

    fn entry(&self, path: &str) -> Option<FileState> {
        self.state.get(&self.session, path)
    }
}

#[tokio::test]
async fn a_written_file_changed_on_disk_gives_its_changes_once() {
    let case = Case::new();
    case.state
        .set(&case.session, "/w/a.txt", written("a\nb\nc\n"));
    case.env.set_file("/w/a.txt", "a\nB\nc\n");
    assert_eq!(
        case.check().await,
        [ChangedFile {
            path: "/w/a.txt".to_owned(),
            snippet: "1\ta\n2\tB\n3\tc".to_owned(),
        }]
    );
    // 重新读进记录，形状同 Read：之后不再查，下一次改动也不提醒。
    let entry = case.entry("/w/a.txt").unwrap();
    assert_eq!(entry.content, "a\nB\nc\n");
    assert_eq!((entry.offset, entry.limit), (Some(1), None));
    assert!(!entry.not_in_context);
    case.env.set_file("/w/a.txt", "again\n");
    assert!(case.check().await.is_empty());
}

/// Read 写下的记录 offset 是 1：从不查。
#[tokio::test]
async fn entries_written_by_read_are_not_checked() {
    let case = Case::new();
    let read = FileState {
        offset: Some(1),
        ..written("a\n")
    };
    case.state.set(&case.session, "/w/a.txt", read);
    case.env.set_file("/w/a.txt", "b\n");
    assert!(case.check().await.is_empty());
    assert_eq!(case.entry("/w/a.txt").unwrap().content, "a\n");
}

/// 版本没变时不读文件、不提醒；版本变了、内容没变时不提醒，但记录改成 Read 的形状。
#[tokio::test]
async fn same_content_is_quiet() {
    let case = Case::new();
    case.env.set_file("/w/a.txt", "same\n");
    case.state.set(&case.session, "/w/a.txt", written("same\n"));
    assert!(case.check().await.is_empty());
    assert_eq!(case.entry("/w/a.txt").unwrap().offset, Some(1));
}

/// 冷恢复的记录没有版本：第一次查时比内容，相同就记下版本、形状不变。
#[tokio::test]
async fn restored_entries_are_compared_by_content_first() {
    let case = Case::new();
    case.env.set_file("/w/a.txt", "same\n");
    case.env.set_file("/w/b.txt", "new\n");
    for (path, content) in [("/w/a.txt", "same\n"), ("/w/b.txt", "old\n")] {
        let restored = FileState {
            baseline: Baseline::Content,
            ..written(content)
        };
        case.state.set(&case.session, path, restored);
    }
    let found = case.check().await;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].path, "/w/b.txt");
    let kept = case.entry("/w/a.txt").unwrap();
    assert_eq!((kept.offset, kept.limit), (None, None));
    assert!(matches!(kept.baseline, Baseline::Version(_)));
    case.env.set_file("/w/a.txt", "changed\n");
    assert_eq!(case.check().await[0].path, "/w/a.txt");
}

#[tokio::test]
async fn a_deleted_file_drops_its_entry() {
    let case = Case::new();
    case.state.set(&case.session, "/w/a.txt", written("a\n"));
    assert!(case.check().await.is_empty());
    assert!(case.entry("/w/a.txt").is_none());
}

/// 记录的内容超过 4096 字节、敏感文件：只说改了，记录标 not_in_context。
#[tokio::test]
async fn large_and_sensitive_files_only_say_they_changed() {
    let case = Case::new();
    let big = "x\n".repeat(2049);
    case.state.set(&case.session, "/w/big.txt", written(&big));
    case.env.set_file("/w/big.txt", "x\n");
    case.state.set(&case.session, "/w/.env", written("A=1\n"));
    case.env.set_file("/w/.env", "A=2\n");
    case.state
        .set(&case.session, "/w/keys/id_ed25519", written("k\n"));
    case.env.set_file("/w/keys/id_ed25519", "k2\n");
    let found = case.check().await;
    let paths: Vec<&str> = found.iter().map(|change| change.path.as_str()).collect();
    assert_eq!(paths, ["/w/keys/id_ed25519", "/w/.env", "/w/big.txt"]);
    assert!(found.iter().all(|change| change.snippet.is_empty()));
    for path in paths {
        assert!(case.entry(path).unwrap().not_in_context, "{path}");
    }
}

/// 只删了行（没有可显示的行）：不提醒。
#[tokio::test]
async fn nothing_to_show_is_not_announced() {
    let case = Case::new();
    case.state.set(&case.session, "/w/a.txt", written("gone\n"));
    case.env.set_file("/w/a.txt", "");
    assert!(case.check().await.is_empty());
}

/// 片段合计到了 16384 之后，后面的文件只说改了。
#[tokio::test]
async fn the_pass_budget_clears_later_snippets() {
    let case = Case::new();
    let long: String = (1..=2000).map(|n| format!("line {n}\n")).collect();
    for name in ["c", "b", "a"] {
        let path = format!("/w/{name}.txt");
        case.state.set(&case.session, &path, written(""));
        case.env.set_file(&path, &long);
    }
    let found = case.check().await;
    let shown: Vec<(&str, bool)> = found
        .iter()
        .map(|change| (change.path.as_str(), change.snippet.is_empty()))
        .collect();
    assert_eq!(
        shown,
        [("/w/a.txt", false), ("/w/b.txt", false), ("/w/c.txt", true)]
    );
    assert!(case.entry("/w/c.txt").unwrap().not_in_context);
    assert!(!case.entry("/w/a.txt").unwrap().not_in_context);
}

#[tokio::test]
async fn images_notebooks_and_pdfs_are_skipped() {
    let case = Case::new();
    for path in ["/w/a.png", "/w/n.ipynb", "/w/d.pdf", "/w/app.exe"] {
        case.state.set(&case.session, path, written("x\n"));
        case.env.set_file(path, "y\n");
    }
    assert!(case.check().await.is_empty());
}

#[test]
fn sensitive_names_follow_the_secret_file_rules() {
    use super::sensitive;
    for path in [
        "/w/.env",
        "/w/.ENV.local",
        "/w/prod.env",
        "/w/server.pem",
        "/w/a/id_rsa",
        "/w/a/id_ed25519_sk",
        "/w/config/secrets.yaml",
        "/w/secret.json",
        "/w/.env. ",
    ] {
        assert!(sensitive(path), "{path}");
    }
    for path in [
        "/w/.env.example",
        "/w/sample.env",
        "/w/env.txt",
        "/w/main.rs",
        "/w/rsa.txt",
    ] {
        assert!(!sensitive(path), "{path}");
    }
}
