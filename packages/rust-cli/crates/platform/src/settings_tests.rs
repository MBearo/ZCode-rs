//! 设置文件的只读视图（platform.md §6）。
use super::*;
use crate::test_support::write;

#[tokio::test]
async fn settings_are_read_from_disk_and_tolerate_bad_files() {
    let dir = tempfile::tempdir().unwrap();
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let settings = Settings::new(paths.settings().clone());
    assert!(settings.read().await.is_empty());
    let file = paths.settings().as_path().to_path_buf();
    write(
        &file,
        r#"{"zcodeEndpointOrigin":" https://zcode.example.com ","theme":"dark"}"#,
    )
    .await;
    assert_eq!(settings.read().await["theme"], "dark");
    assert_eq!(
        settings.zcode_endpoint_origin().await.as_deref(),
        Some("https://zcode.example.com")
    );
    write(&file, "[1]").await;
    assert!(settings.read().await.is_empty());
    write(&file, "{").await;
    assert_eq!(settings.zcode_endpoint_origin().await, None);
}
