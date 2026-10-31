//! 新会话的缺省选择（`/effort` 与登录，tui-manage.md §2.2）：只换 `defaultModelSelection`，其余原样。
use rust_cli_base::models;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_paths::StoragePaths;

use crate::test_support::{read_text, write};

#[tokio::test]
async fn the_effort_default_keeps_the_rest_of_the_personal_config() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let file = paths.provider_config();
    let legacy = paths.cli_config();
    write(
        file.as_path(),
        r#"{"schemaVersion":1,"config":{"providerOrder":["zai"],"defaultModelSelection":{"providerId":"old","modelId":"m"},"x":true}}"#,
    )
    .await;
    let selection = ModelSelection {
        provider: "zai".to_owned(),
        model: models::GLM_5_3_ACCOUNT.to_owned(),
        reasoning: Some("high".to_owned()),
        connection: None,
    };
    let shown = super::super::save_default_model((&file, &legacy), &selection)
        .await
        .unwrap();
    assert_eq!(shown, file.as_path().display().to_string());
    assert_eq!(
        read_text(file.as_path()).await.unwrap(),
        format!(
            "{{\n  \"schemaVersion\": 1,\n  \"config\": {{\n    \"providerOrder\": [\n      \"zai\"\n    ],\n    \"defaultModelSelection\": {{\n      \"providerId\": \"zai\",\n      \"modelId\": \"{}\",\n      \"options\": {{\n        \"reasoningLevel\": \"high\"\n      }}\n    }},\n    \"x\": true\n  }}\n}}",
            models::GLM_5_3_ACCOUNT
        )
    );
    // auto：不带 options。
    let auto = ModelSelection {
        reasoning: None,
        ..selection
    };
    super::super::save_default_model((&file, &legacy), &auto)
        .await
        .unwrap();
    assert!(read_text(file.as_path()).await.unwrap().contains(&format!(
        "\"modelId\": \"{}\"\n    }},",
        models::GLM_5_3_ACCOUNT
    )));
    write(file.as_path(), r#"{"config":{}}"#).await;
    assert!(
        super::super::save_default_model((&file, &legacy), &auto)
            .await
            .is_err()
    );
    assert_eq!(read_text(file.as_path()).await.unwrap(), r#"{"config":{}}"#);
}

/// 个人配置不存在时（修复线 F2）：旧 CLI 用户文件有 Node 要导入的 provider 时不建文件，留给 Node 导入；没有时
/// 从 Node 的空配置开始写。
#[tokio::test]
async fn a_pending_legacy_import_keeps_the_personal_config_absent() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let (file, legacy) = (paths.provider_config(), paths.cli_config());
    write(
        legacy.as_path(),
        r#"{"provider":{"my-proxy":{"kind":"anthropic","options":{"apiKey":"k"}}}}"#,
    )
    .await;
    let selection = ModelSelection {
        provider: "zai".to_owned(),
        model: models::GLM_5_3_ACCOUNT.to_owned(),
        reasoning: None,
        connection: None,
    };
    let refused = super::super::save_default_model((&file, &legacy), &selection)
        .await
        .unwrap_err();
    assert!(
        refused.contains("start the Node CLI or the App once"),
        "{refused}"
    );
    assert_eq!(read_text(file.as_path()).await, None);
    write(legacy.as_path(), r#"{"ui":{"locale":"en-US"}}"#).await;
    super::super::save_default_model((&file, &legacy), &selection)
        .await
        .unwrap();
    assert!(
        read_text(file.as_path())
            .await
            .unwrap()
            .contains("\"providerId\": \"zai\"")
    );
}

/// 团队模型存成缺省时不写团队项目（工作线 F49，platform.md §9.4）：Node 与 App 的 `defaultModelSelection` 是 strict，多一个
/// 字段整份个人配置都不认。
#[tokio::test]
async fn a_team_default_keeps_only_the_node_fields() {
    let dir = tempfile::tempdir().unwrap();
    let paths = StoragePaths::resolve(&Default::default(), dir.path(), dir.path());
    let (file, legacy) = (paths.provider_config(), paths.cli_config());
    write(file.as_path(), r#"{"schemaVersion":1,"config":{}}"#).await;
    let selection = ModelSelection {
        provider: "account:bigmodel-team-coding-plan".to_owned(),
        model: models::GLM_5_3_ACCOUNT.to_owned(),
        reasoning: Some("high".to_owned()),
        connection: Some(Box::new(rust_cli_kernel_api::model::ModelConnection {
            product_id: "prod-1".to_owned(),
            organization_id: "org-t".to_owned(),
            project_id: "p-t".to_owned(),
        })),
    };
    super::super::save_default_model((&file, &legacy), &selection)
        .await
        .unwrap();
    let saved: serde_json::Value =
        serde_json::from_str(&read_text(file.as_path()).await.unwrap()).unwrap();
    assert_eq!(
        saved["config"]["defaultModelSelection"],
        serde_json::json!({"providerId": "account:bigmodel-team-coding-plan", "modelId": models::GLM_5_3_ACCOUNT,
            "options": {"reasoningLevel": "high"}})
    );
}
