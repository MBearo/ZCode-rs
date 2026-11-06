//! 上次的结论（performance.md §12.4）：参数相同才用，选不出时删掉。
use super::*;

use rust_cli_base::models::{GLM_5, GLM_5_3};

fn chosen(model: &str) -> ModelSelection {
    ModelSelection {
        reasoning: Some("max".to_owned()),
        ..ModelSelection::new("bigmodel", model)
    }
}

#[tokio::test]
async fn a_remembered_choice_comes_back_only_for_the_same_flags() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("rust-cli").join("default-model.json");
    let plain: Flags = (None, None);
    assert_eq!(recall(&path, &plain).await, None);
    remember(&path, &plain, &chosen(GLM_5_3)).await;
    assert_eq!(recall(&path, &plain).await, Some(chosen(GLM_5_3)));
    assert_eq!(recall(&path, &(Some(GLM_5.to_owned()), None)).await, None);
    assert_eq!(recall(&path, &(None, Some("low"))).await, None);
}

#[tokio::test]
async fn no_model_forgets_the_last_choice() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default-model.json");
    let flags: Flags = (None, None);
    remember(&path, &flags, &chosen(GLM_5_3)).await;
    remember(
        &path,
        &flags,
        &rust_cli_kernel_api::default_model::unselected(),
    )
    .await;
    assert!(!rust_cli_fs::is_file(&path).await);
    assert_eq!(recall(&path, &flags).await, None);
}

#[tokio::test]
async fn an_unreadable_file_is_no_memory() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("default-model.json");
    rust_cli_fs::append(&path, b"{not json").await.unwrap();
    assert_eq!(recall(&path, &(None, None)).await, None);
}
