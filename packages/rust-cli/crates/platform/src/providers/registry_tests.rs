//! 热更新（platform.md §3.4）：文件不变时沿用快照，个人配置或凭据变化后版本加一。

use rust_cli_base::secret::RedactedString;
use rust_cli_paths::StoragePaths;

use super::*;
use crate::credentials::CredentialSecret;
use crate::test_support::write;

const PERSONAL: &str = include_str!("../../tests/fixtures/personal.json");

async fn registry(dir: &std::path::Path) -> (Registry, StoragePaths) {
    let paths = StoragePaths::resolve(&Default::default(), dir, dir);
    let secret = CredentialSecret {
        explicit: Some(RedactedString::new("s")),
        platform: "linux".to_owned(),
        home: "/home/dev".to_owned(),
        username: "dev".to_owned(),
    };
    let credentials = CredentialStore::new(paths.credentials(), &secret);
    let registry = Registry::new(
        None,
        paths.provider_config(),
        credentials.clone(),
        crate::test_support::offline_account(credentials).await,
    );
    (registry, paths)
}

fn models(current: &Current) -> Vec<String> {
    current
        .snapshot
        .templates
        .iter()
        .map(|template| format!("{}/{}", template.provider, template.model))
        .collect()
}

#[tokio::test]
async fn revisions_change_only_with_the_files() {
    let dir = tempfile::tempdir().unwrap();
    let (registry, paths) = registry(dir.path()).await;
    let first = registry.current().await;
    assert_eq!(first.revision, 1);
    // 没有个人配置、没有登录：随包内置配置里没有可执行的模型。
    assert!(models(&first).is_empty());
    assert_eq!(registry.current().await.revision, 1);

    write(paths.provider_config().as_path(), PERSONAL).await;
    let second = registry.current().await;
    assert_eq!(second.revision, 2);
    assert!(models(&second).contains(&"personal:gateway/glm-5".to_owned()));
    assert_eq!(registry.current().await.revision, 2);

    // 内容相同的改写（签名不变）不加版本。
    write(paths.provider_config().as_path(), PERSONAL).await;
    assert_eq!(registry.current().await.revision, 2);

    // 凭据变化改变账号 overlay：手工 key 登录后账号模型出现。
    let identity = "key-0123456789abcdef01234567";
    let provider = "account:zai-individual-coding-plan";
    let record = serde_json::json!({
        crate::account::identity_key(provider): identity,
        crate::account::source_key(provider): "manual",
        crate::account::manual_key(provider, identity): "sk-manual",
    });
    write(paths.credentials().as_path(), record.to_string()).await;
    let third = registry.current().await;
    assert_eq!(third.revision, 3);
    assert_eq!(models(&third)[0], format!("{provider}/GLM-5.3"));

    // 个人配置删除后回到空配置。
    rust_cli_fs::atomic_write(paths.provider_config().as_path(), b"not json", WRITE)
        .await
        .unwrap();
    let broken = registry.current().await;
    assert_eq!(broken.revision, 4);
    assert!(broken.snapshot.templates.is_empty());
}

const WRITE: rust_cli_fs::WriteOptions = rust_cli_fs::WriteOptions {
    mode: rust_cli_fs::FileMode::Default,
    temp_name: rust_cli_fs::TempName::Hidden,
    parent: rust_cli_fs::ParentDir::Create,
    rename_retry: &[],
};

#[tokio::test]
async fn an_explicit_builtin_file_replaces_the_bundled_copy() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let builtin = dir.path().join("builtin.json");
    write(&builtin, r#"{"schemaVersion":1,"revision":1,"config":{}}"#).await;
    write(paths.provider_config().as_path(), PERSONAL).await;
    let secret = CredentialSecret {
        explicit: None,
        platform: "linux".to_owned(),
        home: "/home/dev".to_owned(),
        username: "dev".to_owned(),
    };
    let paths = paths.with_builtin_provider_config(&builtin);
    let registry = Registry::new(
        paths.builtin_provider_config().cloned(),
        paths.provider_config(),
        CredentialStore::new(paths.credentials(), &secret),
        crate::test_support::offline_account(CredentialStore::new(paths.credentials(), &secret))
            .await,
    );
    // 内置配置为空：个人 provider 没有内置模型规则，模型配置不完整，不可执行。
    assert!(registry.current().await.snapshot.templates.is_empty());
    assert_eq!(registry.current().await.revision, 1);
    // 显式文件不存在：读失败，不发布快照（版本仍为 0），下次照常重读。
    let missing = paths.with_builtin_provider_config(&dir.path().join("missing.json"));
    let registry = Registry::new(
        missing.builtin_provider_config().cloned(),
        missing.provider_config(),
        CredentialStore::new(missing.credentials(), &secret),
        crate::test_support::offline_account(CredentialStore::new(missing.credentials(), &secret))
            .await,
    );
    assert_eq!(registry.current().await.revision, 0);
}
