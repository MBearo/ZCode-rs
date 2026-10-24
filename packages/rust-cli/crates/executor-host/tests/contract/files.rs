//! `EnvInfo` 与 `Files`：条件写与按 operationId 重放、读文本的目录 / 不存在 / 上限、元数据与目录、整份读写、环境事实。

use std::path::{Path, PathBuf};

use rust_cli_env_local::{LocalEnvironment, ShellConfig};
use rust_cli_kernel_api::env::{
    EnvError, EnvPath, FileVersion, ReadFile, WriteFile, WritePrecondition,
};
use rust_cli_kernel_api::env_files::{FileKind, ListDir, PutFile, ReadText, StatFile, TextRead};

use crate::contract;
use crate::support::{Kind, context, env_path, machine, workspace};

fn put(path: &Path, bytes: &[u8], precondition: WritePrecondition) -> PutFile {
    PutFile {
        path: env_path(path),
        bytes: bytes.to_vec(),
        precondition,
        create_parents: true,
    }
}

async fn conditional_writes_check_versions_and_replay_by_operation(kind: Kind) {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace(dir.path());
    let env = machine(kind, LocalEnvironment::new(None)).await;
    let files = env.files();
    let file = dir.path().join("sub/a.txt");
    let created = files
        .put_file(
            &ws,
            context("op_1"),
            put(&file, b"one", WritePrecondition::Absent),
        )
        .await
        .unwrap();
    assert!(created.created);
    // 同一个 operationId、同样的参数：返回第一次的回执，不再写。
    let again = files
        .put_file(
            &ws,
            context("op_1"),
            put(&file, b"one", WritePrecondition::Absent),
        )
        .await
        .unwrap();
    assert_eq!(again, created);
    let conflict = files
        .put_file(
            &ws,
            context("op_1"),
            put(&file, b"two", WritePrecondition::Any),
        )
        .await;
    assert!(matches!(conflict, Err(EnvError::OperationConflict { .. })));
    let old = WritePrecondition::Version(FileVersion::new("old"));
    let stale = files
        .put_file(&ws, context("op_2"), put(&file, b"two", old))
        .await;
    assert!(matches!(stale, Err(EnvError::PreconditionFailed { .. })));
    let current = WritePrecondition::Version(created.version.clone());
    let updated = files
        .put_file(&ws, context("op_3"), put(&file, b"two!", current))
        .await
        .unwrap();
    assert!(!updated.created);
    assert_ne!(updated.version, created.version);
    assert_eq!(std::fs::read(&file).unwrap(), b"two!");
}

async fn reads_report_directories_missing_files_and_size_limits(kind: Kind) {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace(dir.path());
    let env = machine(kind, LocalEnvironment::new(None)).await;
    let files = env.files();
    std::fs::write(dir.path().join("a.txt"), "x\r\ny\n").unwrap();
    let read = |name: &str, max_bytes| ReadText {
        path: env_path(&dir.path().join(name)),
        start_line: 0,
        max_lines: None,
        max_bytes,
    };
    let TextRead::Text(slice) = files.read_text(&ws, read("a.txt", None)).await.unwrap() else {
        panic!("expected text");
    };
    assert_eq!(slice.content, "x\ny\n");
    assert_eq!(slice.total_lines, 3);
    assert_eq!(
        files.read_text(&ws, read("a.txt", Some(2))).await.unwrap(),
        TextRead::TooLarge { size: 5 }
    );
    assert_eq!(
        files.read_text(&ws, read(".", None)).await.unwrap(),
        TextRead::Directory
    );
    assert!(matches!(
        files.read_text(&ws, read("missing", None)).await,
        Err(EnvError::NotFound { .. })
    ));
    let stat = StatFile {
        path: env_path(&dir.path().join("a.txt")),
    };
    let stat = files.stat(&ws, stat).await.unwrap().unwrap();
    assert_eq!((stat.kind, stat.size), (FileKind::File, 5));
    assert_eq!(stat.version, slice.version);
    let missing = StatFile {
        path: env_path(&dir.path().join("missing")),
    };
    assert_eq!(files.stat(&ws, missing).await.unwrap(), None);
    let list = ListDir {
        path: env_path(dir.path()),
    };
    let listed = files.list_dir(&ws, list).await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name, "a.txt");
}

async fn whole_files_round_trip_with_binary_content(kind: Kind) {
    let dir = tempfile::tempdir().unwrap();
    let ws = workspace(dir.path());
    let env = machine(kind, LocalEnvironment::new(None)).await;
    let files = env.files();
    let path = dir.path().join("blob.bin");
    // 比一片（256 KiB）大的二进制内容：远端经二进制段与分片传，不经 JSON。
    let bytes: Vec<u8> = (0..600_000u32).map(|index| (index % 256) as u8).collect();
    let write = WriteFile {
        path: env_path(&path),
        bytes: bytes.clone(),
        precondition: WritePrecondition::Absent,
    };
    let receipt = files.write_file(&ws, context("op_w"), write).await.unwrap();
    let read = files
        .read_file(
            &ws,
            ReadFile {
                path: env_path(&path),
            },
        )
        .await
        .unwrap();
    assert_eq!(read.bytes, bytes);
    assert_eq!(read.version, receipt.version);
    let missing = ReadFile {
        path: env_path(&dir.path().join("nope")),
    };
    assert!(matches!(
        files.read_file(&ws, missing).await,
        Err(EnvError::NotFound { .. })
    ));
}

async fn path_facts_carry_the_home_given_by_the_entry_crate(kind: Kind) {
    let env = machine(
        kind,
        LocalEnvironment::new(Some(PathBuf::from("/home/dev"))),
    )
    .await;
    let facts = env
        .info()
        .path_facts(&workspace(Path::new("/w")))
        .await
        .unwrap();
    assert_eq!(facts.home, Some(EnvPath::new("/home/dev")));
}

async fn facts_describe_the_machine_and_its_workspace(kind: Kind) {
    let home = tempfile::tempdir().unwrap();
    let work = tempfile::tempdir().unwrap();
    let local = LocalEnvironment::new(Some(home.path().to_path_buf()))
        .with_shell(ShellConfig {
            env: vec![("SHELL".to_owned(), "/bin/zsh".to_owned())].into(),
            bash: Some("/bin/bash".into()),
            ..ShellConfig::default()
        })
        .with_additional_directories(vec![EnvPath::new("/extra")]);
    let env = machine(kind, local).await;
    let facts = env.info().facts(&workspace(work.path())).await.unwrap();
    assert_eq!(facts.cwd.as_str(), work.path().to_string_lossy());
    assert_eq!(facts.shell, "zsh");
    assert!(!facts.is_git_repo);
    assert_eq!(facts.additional_directories, [EnvPath::new("/extra")]);
    assert_eq!(
        facts.home.as_ref().map(EnvPath::as_str),
        Some(&*home.path().to_string_lossy())
    );
    assert!(!facts.platform.is_empty() && !facts.os_version.is_empty());
    std::fs::create_dir(work.path().join(".git")).unwrap();
    assert!(
        env.info()
            .facts(&workspace(work.path()))
            .await
            .unwrap()
            .is_git_repo
    );
}

contract!(
    conditional_writes_check_versions_and_replay_by_operation,
    reads_report_directories_missing_files_and_size_limits,
    whole_files_round_trip_with_binary_content,
    path_facts_carry_the_home_given_by_the_entry_crate,
    facts_describe_the_machine_and_its_workspace,
);
