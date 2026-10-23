//! 登录与退出（docs/specs/rust-cli/tui.md §7.2）：账号服务用替身（不连真实服务）。浏览器授权的地址进
//! transcript；esc 发取消、回到选项；授权完成后提示结果并切到新模型；手工 key 只显示 `•`、不进输入历史。
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::services::{CancellationToken, RedactedString};
use rust_cli_tui::{Account, Authorize, Configured, Family, LoginFailure, Services};
use tokio::sync::oneshot;

use super::support::{GatedModel, Setup, with_ui};

const URL: &str = "https://chat.example.com/oauth?flow=1";
const CREDENTIALS: &str = "/Users/dev/.zcode/v2/credentials.json";
const CONFIG: &str = "/Users/dev/.zcode/v2/provider_config.json";

/// 账号服务的替身：浏览器授权等测试放行（`complete`）或取消；记下收到的 key。
#[derive(Default)]
struct FakeAccount {
    release: Mutex<Option<oneshot::Sender<()>>>,
    keys: Mutex<Vec<String>>,
}

impl FakeAccount {
    fn complete(&self) {
        let sender = self.release.lock().unwrap().take().unwrap();
        sender.send(()).unwrap();
    }

    fn keys(&self) -> Vec<String> {
        self.keys.lock().unwrap().clone()
    }
}

fn configured(family: Family, oauth: bool) -> Configured {
    Configured {
        family,
        user: oauth.then(|| "Dev".to_owned()),
        model: "fixture/other-model".to_owned(),
        credentials: oauth.then(|| CREDENTIALS.to_owned()),
        config: CONFIG.to_owned(),
        browser_failed: None,
        oauth,
    }
}

impl Account for FakeAccount {
    fn login(
        &self,
        family: Family,
        authorize: oneshot::Sender<Authorize>,
        cancel: CancellationToken,
    ) -> BoxFuture<'static, Result<Configured, LoginFailure>> {
        let (sender, receiver) = oneshot::channel();
        *self.release.lock().unwrap() = Some(sender);
        let _ = authorize.send(Authorize {
            url: URL.to_owned(),
            browser_failed: None,
        });
        async move {
            tokio::select! {
                () = cancel.cancelled() => Err(LoginFailure::Cancelled),
                _ = receiver => Ok(configured(family, true)),
            }
        }
        .boxed()
    }

    fn configure_api_key(
        &self,
        family: Family,
        key: RedactedString,
    ) -> BoxFuture<'static, Result<Configured, String>> {
        self.keys.lock().unwrap().push(key.expose().to_owned());
        async move { Ok(configured(family, false)) }.boxed()
    }

    fn logout(&self) -> BoxFuture<'static, Result<String, String>> {
        async { Ok(CREDENTIALS.to_owned()) }.boxed()
    }
}

fn setup(account: &Arc<FakeAccount>) -> Setup {
    Setup {
        width: 100,
        height: 30,
        services: Services {
            account: Some(account.clone()),
            ..Services::default()
        },
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn esc_cancels_a_browser_login_and_returns_to_the_choices() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let account = Arc::new(FakeAccount::default());
    with_ui(model, setup(&account), |mut ui| async move {
        ui.submit("/login");
        let screen = ui.wait_for("Set Up Coding Plan").await;
        for option in [
            "Z.AI Coding Plan",
            "BigModel Coding Plan",
            "Z.AI Coding Plan API Key",
            "BigModel Coding Plan API Key",
        ] {
            assert!(screen.contains(option), "{screen}");
        }
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Waiting for Z.AI authorization").await;
        assert!(
            screen.contains("Esc cancels and returns to setup choices."),
            "{screen}"
        );
        let screen = ui.wait_for(URL).await;
        assert!(
            screen.contains("Open this URL to sign in with Z.AI:"),
            "{screen}"
        );
        ui.key(KeyCode::Esc);
        let screen = ui.wait_for("Login cancelled. Choose a setup method.").await;
        assert!(screen.contains("BigModel Coding Plan API Key"), "{screen}");
        assert!(!screen.contains("Configured"), "{screen}");
        ui.key(KeyCode::Esc);
        ui.wait_for("Ask before changes").await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_finished_login_shows_the_result_and_switches_the_model() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let account = Arc::new(FakeAccount::default());
    let control = account.clone();
    with_ui(model, setup(&account), |mut ui| async move {
        ui.submit("/login bigmodel-coding-plan");
        ui.wait_for("Waiting for BigModel authorization").await;
        ui.wait_for(URL).await;
        control.complete();
        let screen = ui.wait_for("Configured BigModel Coding Plan.").await;
        assert!(screen.contains("Model: fixture/other-model"), "{screen}");
        assert!(
            screen.contains(&format!("Model selection: {CONFIG}")),
            "{screen}"
        );
        // 模型目录重读后切到登录给出的模型（footer 显示当前模型）。
        ui.wait("the new model in the footer", |s| {
            s.lines()
                .last()
                .is_some_and(|line| line.contains("other-model"))
        })
        .await;
        ui.submit("/login nope");
        ui.wait_for("Usage: /login [zai-coding-plan|bigmodel-coding-plan")
            .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn api_keys_are_hidden_and_stay_out_of_the_history() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let account = Arc::new(FakeAccount::default());
    with_ui(model, setup(&account), |mut ui| async move {
        ui.submit("/login");
        ui.wait_for("Set Up Coding Plan").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("Enter Z.AI Coding Plan API Key").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("API key is required.").await;
        ui.type_text("sk-secret");
        let screen = ui.wait_for("•••••••••").await;
        assert!(!screen.contains("sk-secret"), "{screen}");
        ui.key(KeyCode::Enter);
        let screen = ui.wait_for("Configured Z.AI Coding Plan.").await;
        assert!(!screen.contains("Credentials:"), "{screen}");
        // 带 key 的一行按 Node 直接保存，但不留在输入历史里。
        ui.submit("/login bigmodel-coding-plan-api-key sk-inline");
        ui.wait_for("Configured BigModel Coding Plan.").await;
        ui.key(KeyCode::Up);
        let screen = ui.wait_for("❯ /login").await;
        assert!(!screen.contains("sk-inline"), "{screen}");
        ui.press(KeyCode::Char('u'), KeyModifiers::CONTROL);
        // 不带 key：直接打开不回显的输入。
        ui.submit("/login bigmodel-coding-plan-api-key");
        ui.wait_for("Enter BigModel Coding Plan API Key").await;
        ui.key(KeyCode::Esc);
        ui.wait_for("API key entry cancelled. Choose a setup method.")
            .await;
    })
    .await;
    assert_eq!(account.keys(), ["sk-secret", "sk-inline"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn logout_reports_the_credentials_file() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let account = Arc::new(FakeAccount::default());
    with_ui(model, setup(&account), |mut ui| async move {
        ui.submit("/logout now");
        ui.wait_for("Usage: /logout").await;
        ui.submit("/logout");
        ui.wait_for(&format!(
            "Logged out from Coding Plan accounts. Credentials: {CREDENTIALS}"
        ))
        .await;
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn without_an_account_service_login_is_unavailable() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.submit("/login zai-coding-plan");
        ui.wait_for("Z.AI Coding Plan login is not available in this client.")
            .await;
        ui.submit("/logout");
        ui.wait_for("Logout is not available in this client.").await;
    })
    .await;
}
