//! 界面偏好的互操作辅助程序（docs/specs/rust-cli/tui-manage.md §2），由 `scripts/rust-cli/node-interop-check.mjs`
//! 调用：
//!
//! - `theme-write <home> <theme>` / `theme-read <home>`：`~/.zcode/cli/config.json` 的 `ui.theme`；
//! - `mcp-write <home> <名字> <true|false>`：用户级配置里 `mcp.servers.<名字>.enabled`（`/mcp enable/disable`）；
//! - `model-write <home> <provider> <model> [<档位>]` / `model-read <home>`：个人 Provider 配置的
//!   `defaultModelSelection`（读法与 platform 的 `default_selection` 相同：`providerId`、`modelId`、
//!   `options.reasoningLevel`）；
//! - `model-initial <home>`：新会话的默认选择，按 Node `resolveInitialModelSelection` 的规则从目录与这份配置算出
//!   （docs/specs/rust-cli/workspace-trust.md §3.1；内置配置用随包副本，没有账号凭据），没有时为 null。
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use rust_cli_base::secret::RedactedString;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_net::{Egress, NetworkPolicy};
use rust_cli_paths::{StoragePaths, StorageSettings};
use rust_cli_platform::{
    AccountOrigins, AccountServices, CredentialSecret, Platform, UiTheme, read_cli_config,
    save_default_model, write_mcp_enabled, write_ui_theme,
};
use serde_json::{Value, json};

#[tokio::main(flavor = "current_thread")]
#[expect(
    clippy::disallowed_methods,
    reason = "互操作辅助程序（不是产品代码）读自己的命令行参数"
)]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let paths = |home: &str| {
        StoragePaths::resolve(
            &StorageSettings::default(),
            Path::new(home),
            Path::new(home),
        )
    };
    let output = match args
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>()
        .as_slice()
    {
        ["theme-write", home, theme] => {
            let theme = UiTheme::parse(theme).context("unknown theme")?;
            let file = write_ui_theme(&paths(home), theme).await?;
            json!({ "path": file.as_path().display().to_string() })
        }
        ["theme-read", home] => {
            let file = read_cli_config(&paths(home)).await;
            let theme = file
                .object()
                .and_then(|object| object.get("ui")?.get("theme")?.as_str())
                .and_then(UiTheme::parse)
                .map(UiTheme::as_str);
            json!({ "theme": theme.map_or(Value::Null, |value| json!(value)) })
        }
        ["model-write", home, provider, model, rest @ ..] => {
            let selection = ModelSelection {
                provider: (*provider).to_owned(),
                model: (*model).to_owned(),
                reasoning: rest.first().map(|level| (*level).to_owned()),
                connection: None,
            };
            let path = save_default_model(&paths(home), &selection)
                .await
                .map_err(anyhow::Error::msg)?;
            json!({ "path": path })
        }
        ["mcp-write", home, name, enabled] => {
            let file = write_mcp_enabled(&paths(home), name, *enabled == "true").await?;
            json!({ "path": file.as_path().display().to_string() })
        }
        ["model-read", home] => {
            let file = paths(home).provider_config();
            let bytes = rust_cli_fs::read_optional(file.as_path())
                .await?
                .context("no provider config")?;
            let value: Value = serde_json::from_slice(&bytes)?;
            let selection = &value["config"]["defaultModelSelection"];
            json!({
                "provider": selection["providerId"],
                "model": selection["modelId"],
                "reasoning": selection["options"]["reasoningLevel"],
            })
        }
        ["model-initial", home] => {
            let platform = offline_platform(&paths(home)).await?;
            match platform.default_selection().await {
                Some(selection) => json!({
                    "provider": selection.provider,
                    "model": selection.model,
                    "reasoning": selection.reasoning,
                }),
                None => Value::Null,
            }
        }
        _ => bail!(
            "usage: ui_prefs theme-write <home> <theme> | theme-read <home> | model-write <home> <provider> <model> [<level>] | model-read <home> | model-initial <home>"
        ),
    };
    print_json(&output);
    Ok(())
}

/// 不连任何服务的平台层：账号服务指向不可达的本地端口；没有凭据，账号 provider 都不授权。
async fn offline_platform(paths: &StoragePaths) -> Result<Platform> {
    struct Fixed;
    impl Clock for Fixed {
        fn now_ms(&self) -> u64 {
            1_700_000_000_000
        }
        fn time_zone(&self) -> String {
            "UTC".to_owned()
        }
    }
    let origin = "http://127.0.0.1:9".to_owned();
    let services = AccountServices {
        egress: Arc::new(Egress::new(NetworkPolicy::default()).await?),
        clock: Arc::new(Fixed),
        origins: AccountOrigins {
            zcode_api: format!("{origin}/api/v1"),
            zai_api: origin.clone(),
            bigmodel_team_plans: format!("{origin}/coding-plan/team/plans"),
            bigmodel_api: origin,
            client_version: "0.0.0-interop".to_owned(),
        },
    };
    let secret = CredentialSecret {
        explicit: Some(RedactedString::new("interop")),
        platform: "linux".to_owned(),
        home: "/home/dev".to_owned(),
        username: "dev".to_owned(),
    };
    Ok(Platform::new(paths, &secret, services))
}

#[expect(clippy::print_stdout, reason = "辅助程序把结果写到标准输出")]
fn print_json(value: &Value) {
    println!("{value}");
}
