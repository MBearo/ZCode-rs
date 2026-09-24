use super::*;
use serde_json::json;

#[test]
fn installed_records_accept_both_node_shapes() {
    let array = parse_installed(&json!({"plugins":[
        {"id":"a@m","name":"a","marketplace":"m","version":"1.0.0","installPath":"/c/a",
            "installedAt":"t","scope":"workspace","source":{"sha":"x"}},
        {"id":"bad","name":"b","marketplace":"m","version":"1","installPath":"/c/b","installedAt":"t","scope":"global"}]}));
    assert_eq!(array.len(), 1);
    assert!(array[0].workspace_scope);
    assert_eq!(array[0].source, Some(json!({"sha":"x"})));
    let map = parse_installed(&json!({"plugins":{
        "b@m":[{"installPath":"/c/b","scope":"project","lastUpdated":"u"},{"installPath":""}],
        "invalid":{"installPath":"/x"}}}));
    assert_eq!(map.len(), 1);
    assert_eq!(
        (
            map[0].name.as_str(),
            map[0].marketplace.as_str(),
            map[0].version.as_str()
        ),
        ("b", "m", "0.0.0")
    );
    assert_eq!(map[0].installed_at, "1970-01-01T00:00:00.000Z");
    assert_eq!(map[0].updated_at.as_deref(), Some("u"));
    assert!(map[0].workspace_scope);
    assert_eq!(split_id("a@b@c"), Some(("a@b", "c")));
    assert_eq!(split_id("@c"), None);
}

#[tokio::test]
async fn bundled_roots_must_stay_inside_the_official_cache() {
    let dir = tempfile::tempdir().unwrap();
    let storage = dir.path();
    let official = crate::official::MARKETPLACE;
    let cache = storage.join("cache").join(official);
    let partition = storage.join("marketplaces").join(official);
    tokio::fs::create_dir_all(&partition).await.unwrap();
    let manifest = json!({"version":1,"manifest":{"plugins":[
        {"name":"a","cachePath":cache.join("a/1.0.0")},
        {"name":"b","cachePath":cache.join("b")},
        {"name":"c","cachePath":storage.join("elsewhere")},
        {"name":"","cachePath":cache.join("x/1")}]}});
    tokio::fs::write(
        partition.join("bundled-marketplace.json"),
        manifest.to_string(),
    )
    .await
    .unwrap();
    assert_eq!(
        bundled_roots(storage).await,
        Some(vec![normalize(&cache.join("a/1.0.0"))])
    );
    tokio::fs::write(
        partition.join("bundled-marketplace.json"),
        "{\"version\":2}",
    )
    .await
    .unwrap();
    // 分片无效时回退扫描缓存目录。
    tokio::fs::create_dir_all(cache.join("p/0.1.0"))
        .await
        .unwrap();
    let mut diagnostics = vec![];
    assert_eq!(
        official_roots(storage, &mut diagnostics).await,
        vec![cache.join("p/0.1.0")]
    );
    assert!(diagnostics.is_empty());
    let home = Path::new("/h");
    assert_eq!(
        storage_root(&json!({"storage":{"dir":"~/.zcode"}}), home),
        PathBuf::from("/h/.zcode/cli/plugins")
    );
    assert_eq!(
        storage_root(&json!({"storage":{"dir":"/d/cli"}}), home),
        PathBuf::from("/d/cli/plugins")
    );
}
