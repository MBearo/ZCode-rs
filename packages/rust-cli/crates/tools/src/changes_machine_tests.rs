//! 按机器分开的“先读后改”与文件变化的提醒（remote-exec.md §20.7.3、§20.7.4）。

use std::sync::Arc;

use rust_cli_kernel_api::env::{EnvFacts, EnvPath, FileVersion, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, SessionId};
use rust_cli_kernel_api::testing::{MemoryEnvironment, TestMachines};
use rust_cli_kernel_api::tool::{ChangedFile, ChangedFiles, ReadFileStates};

use super::FileChanges;
use crate::file_state::{Baseline, FileKey, FileState, ReadFileState};

fn env(root: &str) -> Arc<MemoryEnvironment> {
    Arc::new(MemoryEnvironment::new(EnvFacts {
        cwd: EnvPath::new(root),
        platform: "linux".to_owned(),
        os_version: "Linux 6".to_owned(),
        shell: "bash".to_owned(),
        is_git_repo: false,
        additional_directories: Vec::new(),
        home: None,
    }))
}

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

fn local_workspace() -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new("/w"),
    }
}

#[test]
fn records_are_kept_per_machine() {
    let state = ReadFileState::default();
    let session = SessionId::new("s");
    state.set_at(&session, "dev-a", "/w/a.txt", written("remote\n"));
    // 远端读过不算本机读过（反过来也一样）。
    assert!(state.get(&session, "/w/a.txt").is_none());
    assert_eq!(
        state.get_at(&session, "dev-a", "/w/a.txt").unwrap().content,
        "remote\n"
    );
    state.set(&session, "/w/a.txt", written("local\n"));
    assert_eq!(state.get(&session, "/w/a.txt").unwrap().content, "local\n");
    assert_eq!(
        state.get_at(&session, "dev-a", "/w/a.txt").unwrap().content,
        "remote\n"
    );
    // 本机的清单只有本机的；全部机器的两条都在。
    assert_eq!(state.whole_files(&session).len(), 1);
    let everywhere: Vec<FileKey> = state
        .whole_files_everywhere(&session)
        .into_iter()
        .map(|(key, _)| key)
        .collect();
    // 最近用过的在前：最后一次是读 dev-a 的那条。
    assert_eq!(
        everywhere,
        [
            FileKey::new("dev-a", "/w/a.txt"),
            FileKey::new("local", "/w/a.txt")
        ]
    );
    // 压缩时全部清空，只交出本机的路径去重读。
    assert_eq!(state.clear(&session), ["/w/a.txt"]);
    assert!(state.get_at(&session, "dev-a", "/w/a.txt").is_none());
}

#[tokio::test]
async fn a_remote_machine_gets_one_batched_stat_per_step() {
    let local = env("/w");
    let remote = env("/home/dev/proj");
    let registry = TestMachines::new(local.clone());
    registry.connect("dev-a", remote.clone(), "/home/dev/proj");
    let state = Arc::new(ReadFileState::default());
    let session = SessionId::new("s");
    for (path, content) in [
        ("/home/dev/proj/a.txt", "a\n"),
        ("/home/dev/proj/b.txt", "b\n"),
    ] {
        state.set_at(&session, "dev-a", path, written(content));
        remote.set_file(path, content);
    }
    remote.set_file("/home/dev/proj/b.txt", "B\n");
    local.set_file("/w/l.txt", "l\n");
    state.set(&session, "/w/l.txt", written("l\n"));
    let changes = FileChanges::new(state.clone(), local.clone()).with_machines(registry);
    let found = changes.changed_files(&session, &local_workspace()).await;
    // 远端的路径写成 `<机器>:<路径>`；本机的照旧。
    assert_eq!(
        found,
        [ChangedFile {
            path: "dev-a:/home/dev/proj/b.txt".to_owned(),
            snippet: "1\tB".to_owned(),
        }]
    );
    // 远端：一次批量 stat，没有逐个 stat；本机：逐个 stat（行为不变）。
    assert_eq!(remote.stat_calls(), (0, 1));
    assert_eq!(local.stat_calls(), (1, 0));
    // 改过的那条按 Read 的形状记在 dev-a 上。
    let entry = state
        .get_at(&session, "dev-a", "/home/dev/proj/b.txt")
        .unwrap();
    assert_eq!((entry.content.as_str(), entry.offset), ("B\n", Some(1)));
}

#[tokio::test]
async fn a_disconnected_machine_is_skipped_and_its_records_kept() {
    let local = env("/w");
    let registry = TestMachines::new(local.clone());
    let state = Arc::new(ReadFileState::default());
    let session = SessionId::new("s");
    state.set_at(&session, "dev-a", "/home/dev/proj/a.txt", written("a\n"));
    let changes = FileChanges::new(state.clone(), local.clone()).with_machines(registry);
    assert!(
        changes
            .changed_files(&session, &local_workspace())
            .await
            .is_empty()
    );
    assert!(
        state
            .get_at(&session, "dev-a", "/home/dev/proj/a.txt")
            .is_some()
    );
}

/// 远端执行 F96（remote-exec.md §23.3）：重连中的机器同没连上，每步的检查不问它（它的调用要等重连，会挡住这一步），
/// 记录留着，连回来之后照常查。
#[tokio::test]
async fn a_reconnecting_machine_is_not_asked_and_its_records_kept() {
    let local = env("/w");
    let remote = env("/home/dev/proj");
    let registry = TestMachines::new(local.clone());
    registry.connect("dev-a", remote.clone(), "/home/dev/proj");
    let state = Arc::new(ReadFileState::default());
    let session = SessionId::new("s");
    state.set_at(&session, "dev-a", "/home/dev/proj/a.txt", written("a\n"));
    remote.set_file("/home/dev/proj/a.txt", "A\n");
    let changes = FileChanges::new(state.clone(), local.clone()).with_machines(registry.clone());
    registry.set_reconnecting("dev-a", true);
    assert!(
        changes
            .changed_files(&session, &local_workspace())
            .await
            .is_empty()
    );
    assert_eq!(remote.stat_calls(), (0, 0));
    assert!(
        state
            .get_at(&session, "dev-a", "/home/dev/proj/a.txt")
            .is_some()
    );
    registry.set_reconnecting("dev-a", false);
    let found = changes.changed_files(&session, &local_workspace()).await;
    assert_eq!(found.len(), 1);
    assert_eq!(remote.stat_calls(), (0, 1));
}
