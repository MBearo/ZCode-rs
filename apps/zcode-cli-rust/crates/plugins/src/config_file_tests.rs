use super::*;
use serde_json::json;

async fn text(path: &Path) -> String {
    tokio::fs::read_to_string(path).await.unwrap()
}

#[tokio::test]
async fn edits_keep_key_order_and_follow_node_patches() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    tokio::fs::write(
        &path,
        r#"{"z":1,"plugins":{"enabledPlugins":{"zcode-cua@zcode-plugins-official":true,"a@m":false},"dirs":[]},"a":2}"#,
    )
    .await
    .unwrap();
    set_enabled(&path, "zcode-cua@zcode-plugins-official", false)
        .await
        .unwrap();
    assert_eq!(
        text(&path).await,
        "{\n  \"z\": 1,\n  \"plugins\": {\n    \"enabledPlugins\": {\n      \"a@m\": false,\n      \"computer-use@zcode-plugins-official\": false\n    },\n    \"dirs\": []\n  },\n  \"a\": 2\n}\n"
    );
    let values = json!({"k":"v","n":2}).as_object().cloned().unwrap();
    update_options(&path, "a@m", &values, &[]).await.unwrap();
    let cleared = json!({"n":3}).as_object().cloned().unwrap();
    update_options(&path, "a@m", &cleared, &["k".into()])
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&text(&path).await).unwrap();
    assert_eq!(value["plugins"]["options"]["a@m"], json!({"n":3}));
    remove_enabled(&path, "a@m").await.unwrap();
    let value: Value = serde_json::from_str(&text(&path).await).unwrap();
    assert!(value["plugins"]["enabledPlugins"].get("a@m").is_none());
    assert_eq!(
        value["plugins"]["options"]["a@m"]["n"], 3,
        "options survive"
    );
    remove_plugin(&path, "a@m").await.unwrap();
    let value: Value = serde_json::from_str(&text(&path).await).unwrap();
    assert!(value["plugins"]["options"].get("a@m").is_none());
    add_suppressed(&path, "pdf@zcode-plugins-official")
        .await
        .unwrap();
    add_suppressed(&path, "zcode-cua@zcode-plugins-official")
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&text(&path).await).unwrap();
    assert_eq!(
        value["plugins"]["suppressedBuiltins"],
        json!([
            "pdf@zcode-plugins-official",
            "computer-use@zcode-plugins-official"
        ])
    );
    remove_suppressed(&path, "pdf@zcode-plugins-official")
        .await
        .unwrap();
    let value: Value = serde_json::from_str(&text(&path).await).unwrap();
    assert_eq!(
        value["plugins"]["suppressedBuiltins"],
        json!(["computer-use@zcode-plugins-official"])
    );
    let missing = dir.path().join("new/config.json");
    remove_plugin(&missing, "x@m").await.unwrap();
    assert!(!missing.exists(), "nothing removed, nothing written");
    tokio::fs::write(&path, "[1]").await.unwrap();
    assert!(set_enabled(&path, "a@m", true).await.is_err());
}
