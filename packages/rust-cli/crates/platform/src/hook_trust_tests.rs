//! 信任记录的读写（config.md §4.2）。
#![allow(clippy::disallowed_methods, reason = "测试读回写出的文件与权限")]
use super::*;
use crate::test_support::write;

const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn record(identity: &str, digest: &str, command: &str) -> TrustRecord {
    TrustRecord {
        workspace_identity: identity.to_owned(),
        hook_declaration_digest: digest.to_owned(),
        granted_at: "2026-10-17T08:00:00.000Z".to_owned(),
        last_used_at: None,
        bundle_digest_at_grant: Some(B.to_owned()),
        event_at_grant: "PreToolUse".to_owned(),
        display_command_at_grant: command.to_owned(),
        source_path_at_grant: ".zcode/config.json".to_owned(),
        source_discovery_order_at_grant: Some(0),
        matcher_at_grant: Some(None),
        matcher_index_at_grant: Some(0),
        hook_index_at_grant: Some(1),
        app_version_at_grant: Some("0.1.0".to_owned()),
    }
}

fn store(dir: &std::path::Path) -> HookTrustStore {
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), dir, dir);
    HookTrustStore::new(paths.hook_trust_store(None))
}

#[tokio::test]
async fn grant_writes_the_node_format_and_replaces_records_in_place() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    assert_eq!(store.load().await, TrustLoad::Missing);
    store
        .grant(vec![
            record("/Users/dev/repo", A, "guard.sh"),
            record("/Users/dev/other", A, "x"),
        ])
        .await
        .unwrap();
    store
        .grant(vec![
            record("/Users/dev/repo", A, "guard.sh --v2"),
            record("/Users/dev/repo", B, "y"),
        ])
        .await
        .unwrap();
    let text = tokio::fs::read_to_string(store.path().as_path())
        .await
        .unwrap();
    let expected = format!(
        r#"{{
  "schemaVersion": 1,
  "records": [
    {{
      "workspaceIdentity": "/Users/dev/repo",
      "hookDeclarationDigest": "{A}",
      "digestAlgorithm": "sha256",
      "decision": "trusted",
      "grantedAt": "2026-10-17T08:00:00.000Z",
      "bundleDigestAtGrant": "{B}",
      "eventAtGrant": "PreToolUse",
      "displayCommandAtGrant": "guard.sh --v2",
      "sourcePathAtGrant": ".zcode/config.json",
      "sourceDiscoveryOrderAtGrant": 0,
      "matcherAtGrant": null,
      "matcherIndexAtGrant": 0,
      "hookIndexAtGrant": 1,
      "appVersionAtGrant": "0.1.0"
    }},"#
    );
    assert!(text.starts_with(&expected), "{text}");
    assert!(text.ends_with("}\n  ]\n}\n"));
    assert_eq!(store.load().await.trusted("/Users/dev/repo"), [A, B]);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode =
            |path: &std::path::Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(store.path().as_path()), 0o600);
        assert_eq!(mode(store.path().as_path().parent().unwrap()), 0o700);
    }
    store
        .revoke("/Users/dev/repo", Some(&[B.to_owned()]))
        .await
        .unwrap();
    assert_eq!(store.load().await.trusted("/Users/dev/repo"), [A]);
    store.revoke("/Users/dev/repo", None).await.unwrap();
    assert!(store.load().await.trusted("/Users/dev/repo").is_empty());
    assert_eq!(store.load().await.trusted("/Users/dev/other"), [A]);
}

#[tokio::test]
async fn invalid_files_fail_closed_and_are_not_overwritten() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let file = store.path().as_path().to_path_buf();
    let good = record("/w", A, "x").to_value();
    let mut extra = good.clone();
    extra["unknown"] = json!(1);
    let mut wrong_event = good.clone();
    wrong_event["eventAtGrant"] = json!("Notification");
    let mut bad_time = good.clone();
    bad_time["grantedAt"] = json!("yesterday");
    for records in [
        json!([extra]),
        json!([wrong_event]),
        json!([bad_time]),
        json!([good, good]),
    ] {
        let text = json!({"schemaVersion": 1, "records": records}).to_string();
        write(&file, &text).await;
        assert_eq!(store.load().await, TrustLoad::Corrupt);
        assert!(matches!(
            store.grant(vec![record("/w", B, "y")]).await,
            Err(TrustError::Corrupt)
        ));
        assert_eq!(tokio::fs::read_to_string(&file).await.unwrap(), text);
    }
    write(&file, "{").await;
    assert_eq!(store.load().await, TrustLoad::Corrupt);
    // 没有 matcherAtGrant 等可选字段的旧记录照常能读。
    let minimal = json!({"schemaVersion": 1, "records": [{
        "workspaceIdentity": "/w", "hookDeclarationDigest": A, "digestAlgorithm": "sha256",
        "decision": "trusted", "grantedAt": "2026-10-17T08:00:00Z", "eventAtGrant": "Stop",
        "displayCommandAtGrant": "x", "sourcePathAtGrant": "zcode.json"
    }]});
    write(&file, &minimal.to_string()).await;
    assert_eq!(store.load().await.trusted("/w"), [A]);
}
