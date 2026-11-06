//! TUI 的账号服务（docs/specs/rust-cli/tui.md §7.2）：platform 的登录、退出与家族的账号状态，加上打开浏览器（与 `zcode login`
//! 共用 [`crate::commands::login::open_browser`]）。TUI 不起进程，打开浏览器在这里做。
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_base::account::Family;
use rust_cli_base::secret::RedactedString;
use rust_cli_platform::usage::AccountPlan;
use rust_cli_platform::{LoginError, Platform};
use rust_cli_tui_ports::services::CancellationToken;
use rust_cli_tui_ports::services::{
    Account, Authorize, Configured, FamilyModel, FamilyStatus, LoginFailure, PlanConnection,
    TeamStatus,
};
use tokio::sync::oneshot;

pub(crate) struct PlatformAccount {
    pub(crate) platform: Arc<Platform>,
}

fn failure(error: LoginError) -> LoginFailure {
    match error {
        LoginError::Cancelled => LoginFailure::Cancelled,
        other => LoginFailure::Failed(other.to_string()),
    }
}

impl Account for PlatformAccount {
    fn login(
        &self,
        family: Family,
        authorize: oneshot::Sender<Authorize>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Configured, LoginFailure>> {
        let platform = self.platform.clone();
        async move {
            let pending = platform.begin_login(family).await.map_err(failure)?;
            let url = pending.authorize_url().to_owned();
            let browser_failed = crate::commands::login::open_browser(&url).err();
            // 界面可能已经不等地址了（退出中）：发不出去不影响登录。
            let _ = authorize.send(Authorize {
                url,
                browser_failed: browser_failed.clone(),
            });
            let outcome = pending.complete_until(&cancel).await.map_err(failure)?;
            Ok(Configured {
                family: outcome.family,
                user: outcome.user_label().map(str::to_owned),
                model: outcome.model,
                credentials: Some(outcome.credentials.as_path().display().to_string()),
                config: outcome.config.as_path().display().to_string(),
                browser_failed,
                oauth: true,
            })
        }
        .boxed()
    }

    fn configure_api_key(
        &self,
        family: Family,
        key: RedactedString,
    ) -> BoxFuture<'static, Result<Configured, String>> {
        let platform = self.platform.clone();
        async move {
            let outcome = platform
                .configure_api_key(family, &key)
                .await
                .map_err(|error| error.to_string())?;
            Ok(Configured {
                family: outcome.family,
                user: None,
                model: outcome.model,
                credentials: None,
                config: outcome.config.as_path().display().to_string(),
                browser_failed: None,
                oauth: false,
            })
        }
        .boxed()
    }

    fn logout(&self) -> BoxFuture<'static, Result<String, String>> {
        let platform = self.platform.clone();
        async move {
            platform
                .logout()
                .await
                .map(|file| file.as_path().display().to_string())
                .map_err(|error| error.to_string())
        }
        .boxed()
    }

    /// 内置家族的账号状态（platform `family_accounts`）：快照里的团队项目都拿得到密钥（可用）。
    fn families(&self) -> BoxFuture<'static, Vec<FamilyStatus>> {
        let platform = self.platform.clone();
        async move {
            platform
                .family_accounts()
                .await
                .into_iter()
                .map(|f| FamilyStatus {
                    family: f.family,
                    name: match f.family {
                        Family::Zai => "Z.ai",
                        Family::Bigmodel => "BigModel",
                    }
                    .to_owned(),
                    signed_in: f.signed_in,
                    account: f.account,
                    connection: match f.plan {
                        AccountPlan::Start => PlanConnection::Start,
                        AccountPlan::Individual => PlanConnection::Individual,
                        AccountPlan::Team(i) => PlanConnection::Team(i),
                    },
                    teams: f
                        .teams
                        .into_iter()
                        .map(|t| TeamStatus {
                            name: t.name,
                            usable: true,
                            why: String::new(),
                        })
                        .collect(),
                    teams_error: String::new(),
                    provider: f.provider,
                    models: f
                        .models
                        .into_iter()
                        .map(|m| FamilyModel {
                            id: m.model,
                            enabled: m.enabled,
                            context_window: m.context_window,
                        })
                        .collect(),
                })
                .collect()
        }
        .boxed()
    }
}
