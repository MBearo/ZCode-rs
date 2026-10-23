//! TUI 的账号服务（docs/specs/rust-cli/tui.md §7.2）：platform 的登录与退出，加上打开浏览器（与 `zcode login`
//! 共用 [`crate::login::open_browser`]）。TUI 不起进程，打开浏览器在这里做。
use std::sync::Arc;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_base::secret::RedactedString;
use rust_cli_platform::{Family, LoginError, Platform};
use rust_cli_tui::services::CancellationToken;
use rust_cli_tui::{Account, Authorize, Configured, LoginFailure};
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
            let browser_failed = crate::login::open_browser(&url).err();
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
}
