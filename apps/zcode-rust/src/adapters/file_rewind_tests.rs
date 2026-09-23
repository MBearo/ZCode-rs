use super::*;
async fn fixture() -> (tempfile::TempDir, Vec<FileCheckpoint>, Arc<Mutex<()>>) {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("source.txt");
    let before = blobs::save(root.path(), b"\xef\xbb\xbforiginal\r\n")
        .await
        .unwrap();
    let after = blobs::save(root.path(), b"changed\r\n").await.unwrap();
    tokio::fs::write(&path, b"changed\r\n").await.unwrap();
    let path = tokio::fs::canonicalize(path).await.unwrap();
    (
        root,
        vec![FileCheckpoint {
            id: "checkpoint".into(),
            path: path.to_string_lossy().into_owned(),
            tool: "Edit".into(),
            before: Some(before),
            after,
            mode: Some(0o100640),
            row: 3,
            restored: false,
        }],
        Arc::new(Mutex::new(())),
    )
}
#[tokio::test]
async fn journal_recovers_crash_on_each_side_of_the_database_commit_and_keeps_raw_bytes() {
    for committed in [false, true] {
        let (root, changes, gate) = fixture().await;
        let tx = begin(root.path(), "session", "op", &changes, gate.clone())
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(&changes[0].path).await.unwrap(),
            b"\xef\xbb\xbforiginal\r\n"
        );
        drop(tx); // 模拟进程在文件恢复之后、数据库提交前后退出。
        assert_eq!(pending(root.path()).await.unwrap(), vec!["session"]);
        recover(root.path(), "session", committed.then_some("op"), gate)
            .await
            .unwrap();
        assert_eq!(
            tokio::fs::read(&changes[0].path).await.unwrap(),
            if committed {
                b"\xef\xbb\xbforiginal\r\n".as_slice()
            } else {
                b"changed\r\n".as_slice()
            }
        );
        assert!(pending(root.path()).await.unwrap().is_empty());
        #[cfg(unix)]
        if committed {
            assert_eq!(
                blobs::mode(Path::new(&changes[0].path))
                    .await
                    .unwrap()
                    .unwrap()
                    & 0o777,
                0o640
            );
        }
    }
}
#[tokio::test]
async fn recovery_never_overwrites_external_modification_and_keeps_journal() {
    let (root, changes, gate) = fixture().await;
    let tx = begin(root.path(), "session", "op", &changes, gate.clone())
        .await
        .unwrap();
    drop(tx);
    tokio::fs::write(&changes[0].path, b"external")
        .await
        .unwrap();
    assert!(
        recover(root.path(), "session", None, gate.clone())
            .await
            .is_err()
    );
    assert_eq!(
        tokio::fs::read(&changes[0].path).await.unwrap(),
        b"external"
    );
    assert_eq!(pending(root.path()).await.unwrap(), vec!["session"]);
    tokio::fs::write(&changes[0].path, b"\xef\xbb\xbforiginal\r\n")
        .await
        .unwrap();
    recover(root.path(), "session", None, gate).await.unwrap();
}
#[tokio::test]
async fn corrupt_or_missing_blob_cannot_apply_and_dedup_is_content_addressed() {
    let (root, changes, _) = fixture().await;
    let key = changes[0].before.as_ref().unwrap();
    let path = blobs::path(root.path(), key).unwrap();
    assert_eq!(
        blobs::save(root.path(), b"\xef\xbb\xbforiginal\r\n")
            .await
            .unwrap(),
        *key
    );
    tokio::fs::write(&path, b"corruption").await.unwrap();
    let p = preview(root.path(), &changes).await.unwrap();
    assert_eq!(p["unsafeFiles"][0]["reason"], "checkpoint_unreadable");
    tokio::fs::remove_file(path).await.unwrap();
    let p = preview(root.path(), &changes).await.unwrap();
    assert_eq!(p["unsafeFiles"][0]["reason"], "checkpoint_missing");
}
