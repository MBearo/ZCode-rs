#![expect(clippy::disallowed_methods, reason = "测试直接准备工作区夹具")]

use std::fs;
use std::path::Path;

use rust_cli_kernel_api::env::{FileVersion, Origin, WritePrecondition};
use rust_cli_kernel_api::env_files::FileKind;
use rust_cli_kernel_api::ids::{EnvironmentId, Generation, OwnerEpoch, RunId};

use super::*;

fn workspace(root: &Path) -> WorkspaceRef {
    WorkspaceRef {
        environment: EnvironmentId::new("local"),
        root: EnvPath::new(root.to_string_lossy()),
    }
}

fn context(operation: &str) -> OperationContext {
    OperationContext {
        environment: EnvironmentId::new("local"),
        generation: Generation::new("g1"),
        owner_epoch: OwnerEpoch::LOCAL,
        operation: OperationId::new(operation),
        origin: Origin::Agent {
            session: SessionId::new("sess_1"),
            run: RunId::new("run_1"),
        },
    }
}

fn env_path(path: &Path) -> EnvPath {
    EnvPath::new(path.to_string_lossy())
}

fn put(path: &Path, bytes: &[u8], precondition: WritePrecondition) -> PutFile {
    PutFile {
        path: env_path(path),
        bytes: bytes.to_vec(),
        precondition,
        create_parents: true,
    }
}

#[tokio::test]
async fn conditional_writes_check_versions_and_replay_by_operation() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace(dir.path());
    let env = LocalEnvironment::new(None);
    let file = dir.path().join("sub/a.txt");
    let created = env
        .put_file(
            &ws,
            context("op_1"),
            put(&file, b"one", WritePrecondition::Absent),
        )
        .await
        .unwrap();
    assert!(created.created);
    // 同一个 operationId、同样的参数：返回第一次的回执，不再写。
    let again = env
        .put_file(
            &ws,
            context("op_1"),
            put(&file, b"one", WritePrecondition::Absent),
        )
        .await
        .unwrap();
    assert_eq!(again, created);
    let conflict = env
        .put_file(
            &ws,
            context("op_1"),
            put(&file, b"two", WritePrecondition::Any),
        )
        .await;
    assert!(matches!(conflict, Err(EnvError::OperationConflict { .. })));
    let stale = env
        .put_file(
            &ws,
            context("op_2"),
            put(
                &file,
                b"two",
                WritePrecondition::Version(FileVersion::new("old")),
            ),
        )
        .await;
    assert!(matches!(stale, Err(EnvError::PreconditionFailed { .. })));
    let updated = env
        .put_file(
            &ws,
            context("op_3"),
            put(
                &file,
                b"two!",
                WritePrecondition::Version(created.version.clone()),
            ),
        )
        .await
        .unwrap();
    assert!(!updated.created);
    assert_ne!(updated.version, created.version);
    assert_eq!(fs::read(&file).unwrap(), b"two!");
}

#[tokio::test]
async fn reads_report_directories_missing_files_and_size_limits() {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace(dir.path());
    let env = LocalEnvironment::new(None);
    fs::write(dir.path().join("a.txt"), "x\r\ny\n").unwrap();
    let read = |name: &str, max_bytes| ReadText {
        path: env_path(&dir.path().join(name)),
        start_line: 0,
        max_lines: None,
        max_bytes,
    };
    let TextRead::Text(slice) = env.read_text(&ws, read("a.txt", None)).await.unwrap() else {
        panic!("expected text");
    };
    assert_eq!(slice.content, "x\ny\n");
    assert_eq!(slice.total_lines, 3);
    assert_eq!(
        env.read_text(&ws, read("a.txt", Some(2))).await.unwrap(),
        TextRead::TooLarge { size: 5 }
    );
    assert_eq!(
        env.read_text(&ws, read(".", None)).await.unwrap(),
        TextRead::Directory
    );
    assert!(matches!(
        env.read_text(&ws, read("missing", None)).await,
        Err(EnvError::NotFound { .. })
    ));
    let stat = env
        .stat(
            &ws,
            StatFile {
                path: env_path(&dir.path().join("a.txt")),
            },
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!((stat.kind, stat.size), (FileKind::File, 5));
    assert_eq!(stat.version, slice.version);
    let missing = StatFile {
        path: env_path(&dir.path().join("missing")),
    };
    assert_eq!(env.stat(&ws, missing).await.unwrap(), None);
    let listed = env
        .list_dir(
            &ws,
            ListDir {
                path: env_path(dir.path()),
            },
        )
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "a.txt");
}

#[tokio::test]
async fn path_facts_carry_the_home_given_by_the_entry_crate() {
    let env = LocalEnvironment::new(Some(PathBuf::from("/home/dev")));
    let facts = env.path_facts(&workspace(Path::new("/w"))).await.unwrap();
    assert_eq!(facts.home, Some(EnvPath::new("/home/dev")));
}
