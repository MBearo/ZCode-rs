use super::*;

async fn write(path: &Path, text: &str) {
    tokio::fs::create_dir_all(path.parent().unwrap())
        .await
        .unwrap();
    tokio::fs::write(path, text).await.unwrap();
}

async fn read(path: &Path) -> String {
    tokio::fs::read_to_string(path).await.unwrap_or_default()
}

fn input<'a>(
    target: &'a Path,
    source: Option<&'a Path>,
    authority: Option<&'a Path>,
    cancel: &'a CancellationToken,
) -> Input<'a> {
    Input {
        target,
        source,
        authority,
        cancel,
    }
}

#[tokio::test]
async fn activation_commits_finalizes_and_rolls_back_like_node() {
    let dir = tempfile::tempdir().unwrap();
    let (source, target) = (dir.path().join("src"), dir.path().join("cache/t"));
    let authority = dir.path().join("installed.json");
    write(&source.join("a/f.txt"), "new").await;
    #[cfg(unix)]
    tokio::fs::symlink("a/f.txt", source.join("link"))
        .await
        .unwrap();
    write(&target.join("f.txt"), "old").await;
    write(&authority, "{}").await;
    let cancel = CancellationToken::new();
    let activation = activate(
        input(&target, Some(&source), Some(&authority), &cancel),
        |staged| async move {
            tokio::fs::write(staged.join("prepared"), "yes").await?;
            Ok(())
        },
    )
    .await
    .unwrap();
    let (backup, marker) = sidecars(&target);
    assert_eq!(read(&target.join("a/f.txt")).await, "new");
    assert_eq!(read(&target.join("prepared")).await, "yes");
    assert_eq!(read(&backup.join("f.txt")).await, "old");
    let record: serde_json::Value = serde_json::from_str(&read(&marker).await).unwrap();
    assert_eq!(record["mode"], "coordinated");
    assert_eq!(record["transactionId"], activation.transaction_id.as_str());
    #[cfg(unix)]
    assert_eq!(
        tokio::fs::read_link(target.join("link")).await.unwrap(),
        source.join("a/f.txt"),
        "relative links resolve to absolute targets (Node verbatimSymlinks: false)"
    );
    // 活跃事务期间，权威文件尚未记录该事务：读取方看到上一代（backup）。
    assert_eq!(recover(&target).await.unwrap(), backup);
    // 同一目标的第二个事务报错。
    let second = activate(input(&target, None, None, &cancel), |_| async { Ok(()) }).await;
    assert!(second.err().unwrap().to_string().contains("already active"));
    activation.rollback().await.unwrap();
    assert_eq!(read(&target.join("f.txt")).await, "old");
    assert!(!crate::fsx::exists(&marker).await && !crate::fsx::exists(&backup).await);

    let activation = activate(input(&target, Some(&source), None, &cancel), |_| async {
        Ok(())
    })
    .await
    .unwrap();
    activation.finalize().await;
    assert_eq!(read(&target.join("a/f.txt")).await, "new");
    assert!(!crate::fsx::exists(&marker).await && !crate::fsx::exists(&backup).await);
    let leftovers: Vec<String> = crate::fsx::read_dir_names(target.parent().unwrap())
        .await
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(leftovers, vec!["t"], "the staging container is removed");
}

#[tokio::test]
async fn cancellation_and_prepare_failures_keep_the_target() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("t");
    write(&target.join("f"), "old").await;
    let cancel = CancellationToken::new();
    let failed = activate(input(&target, None, None, &cancel), |_| async {
        anyhow::bail!("prepare failed")
    })
    .await;
    assert_eq!(failed.err().unwrap().to_string(), "prepare failed");
    let token = cancel.clone();
    let cancelled = activate(input(&target, None, None, &cancel), |_| async move {
        token.cancel();
        Ok(())
    })
    .await;
    assert_eq!(
        cancelled.err().unwrap().to_string(),
        crate::failure::CANCELLED
    );
    assert_eq!(read(&target.join("f")).await, "old");
    let names: Vec<String> = crate::fsx::read_dir_names(dir.path())
        .await
        .unwrap()
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    assert_eq!(names, vec!["t"]);
    // 失败后登记已解除，可以再次激活。
    let fresh = CancellationToken::new();
    activate(input(&target, None, None, &fresh), |_| async { Ok(()) })
        .await
        .unwrap()
        .finalize()
        .await;
    assert!(!crate::fsx::exists(&target.join("f")).await);
}
