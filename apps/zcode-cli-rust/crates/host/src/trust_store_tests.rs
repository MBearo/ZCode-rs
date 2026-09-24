use super::*;

fn record(identity: &str, digest: char) -> Record {
    trust::parse_record(&serde_json::json!({
        "workspaceIdentity": identity,
        "hookDeclarationDigest": digest.to_string().repeat(64),
        "digestAlgorithm": "sha256",
        "decision": "trusted",
        "grantedAt": "2026-01-01T00:00:00.000Z",
        "eventAtGrant": "Stop",
        "displayCommandAtGrant": "x",
        "sourcePathAtGrant": ".zcode/config.json",
        "matcherAtGrant": null
    }))
    .unwrap()
}

#[tokio::test]
async fn resolves_the_store_under_the_user_storage_dir() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path();
    let config = home.join("config.json");
    let path = |dir: &str| {
        let config = config.clone();
        let text = serde_json::json!({"storage":{"dir":dir}}).to_string();
        async move {
            tokio::fs::write(&config, text).await.unwrap();
            trust_store_path(home, &config).await.unwrap()
        }
    };
    let suffix = Path::new("security").join("workspace-hook-trust-v1.json");
    assert_eq!(
        trust_store_path(home, &config).await.unwrap(),
        home.join(".zcode").join(&suffix)
    );
    assert_eq!(path("  ~/data ").await, home.join("data").join(&suffix));
    assert_eq!(path("/abs").await, Path::new("/abs").join(&suffix));
    assert_eq!(path("rel").await, home.join("rel").join(&suffix));
    assert_eq!(path("  ").await, home.join(".zcode").join(&suffix));
    tokio::fs::write(&config, "{bad").await.unwrap();
    assert!(trust_store_path(home, &config).await.is_err());
}

#[tokio::test]
async fn grants_revokes_and_writes_like_node() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("security").join("trust.json");
    let store = FileTrustStore::new(file.clone());
    assert_eq!(store.load().await.unwrap(), TrustLoad::Records(vec![]));
    let (a, b, c) = (record("w", 'a'), record("w", 'b'), record("other", 'c'));
    store.grant(vec![a.clone(), b.clone()]).await.unwrap();
    let mut updated = a.clone();
    updated.display_command_at_grant = "y".into();
    // upsert 保留已有位置，新记录追加在末尾。
    let records = store.grant(vec![c.clone(), updated.clone()]).await.unwrap();
    assert_eq!(records, [updated.clone(), b.clone(), c.clone()]);
    let content = tokio::fs::read_to_string(&file).await.unwrap();
    assert_eq!(content, trust::store_content(&records));
    assert_eq!(store.load().await.unwrap(), TrustLoad::Records(records));
    let left = store.revoke("w", Some(vec!["b".repeat(64)])).await.unwrap();
    assert_eq!(left, [updated, c.clone()]);
    assert_eq!(store.revoke("w", None).await.unwrap(), [c]);
    assert!(store.revoke("w", Some(vec![])).await.is_err());
    assert!(
        !tokio::fs::try_exists(root.path().join("security/trust.json.lock"))
            .await
            .unwrap()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&file), 0o600);
        assert_eq!(mode(file.parent().unwrap()), 0o700);
    }
}

#[tokio::test]
async fn corrupt_files_are_moved_aside_and_replaced() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("trust.json");
    tokio::fs::write(&file, "{\"schemaVersion\":1,\"records\":[{}]}")
        .await
        .unwrap();
    let store = FileTrustStore::new(file.clone());
    assert_eq!(store.load().await.unwrap(), TrustLoad::Corrupt);
    assert!(!tokio::fs::try_exists(&file).await.unwrap());
    let mut names = tokio::fs::read_dir(root.path()).await.unwrap();
    let mut aside = vec![];
    while let Some(entry) = names.next_entry().await.unwrap() {
        aside.push(entry.file_name().to_string_lossy().into_owned());
    }
    assert!(
        aside.iter().any(|n| n.starts_with("trust.json.corrupt-")),
        "{aside:?}"
    );
    tokio::fs::write(&file, "not json").await.unwrap();
    // 损坏文件在写入时按空记录处理并被覆盖。
    let records = store.grant(vec![record("w", 'a')]).await.unwrap();
    assert_eq!(store.load().await.unwrap(), TrustLoad::Records(records));
}

#[tokio::test]
async fn waits_for_live_locks_and_reclaims_abandoned_ones() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("trust.json");
    let lock = root.path().join("trust.json.lock");
    let store = FileTrustStore::new(file.clone());
    // 另一持有者的新锁：等待其释放。
    tokio::fs::write(&lock, "{\"pid\":1,\"startTime\":0,\"token\":\"t\"}\n")
        .await
        .unwrap();
    let held = lock.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        tokio::fs::remove_file(held).await.unwrap();
    });
    let started = std::time::Instant::now();
    store.grant(vec![record("w", 'a')]).await.unwrap();
    assert!(started.elapsed() >= Duration::from_millis(100));
    // 超过 30 s、持有进程已不存在的锁被回收。
    tokio::fs::write(
        &lock,
        "{\"pid\":2147483646,\"startTime\":0,\"token\":\"t\"}\n",
    )
    .await
    .unwrap();
    let old = std::time::SystemTime::now() - Duration::from_secs(60);
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(old)
        .unwrap();
    assert!(matches!(store.load().await.unwrap(), TrustLoad::Records(r) if r.len() == 1));
    // 无法解析的旧锁同样按无主回收。
    tokio::fs::write(&lock, "").await.unwrap();
    std::fs::File::options()
        .write(true)
        .open(&lock)
        .unwrap()
        .set_modified(old)
        .unwrap();
    store.load().await.unwrap();
}
