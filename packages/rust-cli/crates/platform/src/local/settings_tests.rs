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

/// App 的“完整保留模型 I/O”开关（工作线 F91）：只有布尔的 `true` 算开，每次读都以磁盘为准。
#[tokio::test]
async fn the_full_model_io_retention_switch_is_read_each_time() {
    let dir = tempfile::tempdir().unwrap();
    let paths = rust_cli_paths::StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let settings = Settings::new(paths.settings().clone());
    assert!(!settings.model_io_full_retention().await);
    let file = paths.settings().as_path().to_path_buf();
    write(&file, r#"{"modelIoFullRetentionEnabled":true}"#).await;
    assert!(settings.model_io_full_retention().await);
    write(&file, r#"{"modelIoFullRetentionEnabled":"true"}"#).await;
    assert!(!settings.model_io_full_retention().await);
    write(&file, r#"{"modelIoFullRetentionEnabled":false}"#).await;
    assert!(!settings.model_io_full_retention().await);
}
