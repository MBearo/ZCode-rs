//! 登录与退出（docs/specs/rust-cli/tui.md §7.2）：账号服务用替身（不连真实服务）。`/login` 打开 Puffin 的门道
//! （tui-kit.md §4.2「登录」）：浏览器授权的地址在门道里，`c` 复制；esc 发取消、回到选择；授权完成后门道关上、说接上了
//! 什么并切到新模型；手工 key 在板上只显示 `•`、不进输入历史。`/logout` 停在 Settings › Models › 登录了的家族的 Sign out 行。
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::services::{CancellationToken, RedactedString};
use rust_cli_tui::{
    Account, Authorize, Configured, Family, FamilyStatus, LoginFailure, PlanConnection, Services,
};
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
    signed_out: Mutex<bool>,
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
        *self.signed_out.lock().unwrap() = true;
        async { Ok(CREDENTIALS.to_owned()) }.boxed()
    }

    /// 登录着 Z.ai（退出之后不再是）。
    fn families(&self) -> BoxFuture<'static, Vec<FamilyStatus>> {
        let signed_in = !*self.signed_out.lock().unwrap();
        async move {
            vec![FamilyStatus {
                family: Family::Zai,
                name: "Z.ai".to_owned(),
                signed_in,
                account: "dev@example.com".to_owned(),
                connection: PlanConnection::Individual,
                teams: Vec::new(),
                teams_error: String::new(),
                provider: "fixture".to_owned(),
                models: Vec::new(),
            }]
        }
        .boxed()
    }
}

fn setup(account: &Arc<FakeAccount>) -> Setup {
    Setup {
        width: 100,
        height: 30,
        services: Services {
            account: account.clone(),
            ..super::stub_services::services()
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
        let screen = ui.wait_for("Connect a model to start.").await;
        for option in [
            "1  Sign in to Z.ai",
            "2  Sign in to BigModel",
            "3  Use an API key",
        ] {
            assert!(screen.contains(option), "{screen}");
        }
        ui.key(KeyCode::Enter);
        ui.wait_for("Waiting for you to sign in").await;
        ui.wait_for("Opened the sign-in page in your browser.")
            .await;
        // `c` 复制网址：槽里说复制了。
        ui.key(KeyCode::Char('c'));
        ui.wait_for("Copied the link").await;
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait("back at the choices", |s| {
                s.contains("Connect a model to start.") && !s.contains("Waiting for you")
            })
            .await;
        assert!(!screen.contains("Signed in"), "{screen}");
        // 已有模型：esc 关上门道。
        ui.key(KeyCode::Esc);
        ui.wait("the door closed", |s| !s.contains("Sign in to Z.ai"))
            .await;
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
        ui.wait_for("Waiting for you to sign in").await;
        ui.wait_for("Opened the sign-in page in your browser.")
            .await;
        control.complete();
        let screen = ui.wait_for("✓ Signed in to BigModel as Dev.").await;
        assert!(!screen.contains("Credentials"), "{screen}");
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
        ui.wait_for("Connect a model to start.").await;
        ui.key(KeyCode::Char('3'));
        ui.wait_for("Which service is the key for?").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Paste your Z.ai key below.").await;
        ui.type_text("sk-secret");
        let screen = ui.wait_for("•••••••••").await;
        assert!(!screen.contains("sk-secret"), "{screen}");
        ui.key(KeyCode::Enter);
        ui.wait_for("✓ Connected to Z.ai with your key. other-model answered in 0.0 s.")
            .await;
        // 带 key 的一行按 Node 直接保存，但不留在输入历史里。
        ui.submit("/login bigmodel-coding-plan-api-key sk-inline");
        ui.wait_for("✓ Connected to BigModel with your key. other-model answered in 0.0 s.")
            .await;
        ui.key(KeyCode::Up);
        let screen = ui.wait_for("› /login").await;
        assert!(!screen.contains("sk-inline"), "{screen}");
        ui.press(KeyCode::Char('u'), KeyModifiers::CONTROL);
        // 不带 key：门道直接到粘贴，esc 回到给哪个服务。
        ui.submit("/login bigmodel-coding-plan-api-key");
        ui.wait_for("Paste your BigModel key below.").await;
        ui.key(KeyCode::Esc);
        ui.wait_for("Which service is the key for?").await;
    })
    .await;
    assert_eq!(account.keys(), ["sk-secret", "sk-inline"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn logout_opens_the_sign_out_row_and_signs_out() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let account = Arc::new(FakeAccount::default());
    with_ui(model, setup(&account), |mut ui| async move {
        ui.submit("/logout now");
        ui.wait_for("Usage: /logout").await;
        // `/logout`：Settings › Models › 登录了的家族，停在 Sign out 行（tui-kit.md §4.3）。
        ui.submit("/logout");
        ui.wait("the family page", |s| {
            s.contains("Settings  ›  Models  ›  Z.ai")
                && s.contains("dev@example.com")
                && s.contains("The desktop app is signed out too: they share one account.")
        })
        .await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Signed out. The desktop app is signed out too.")
            .await;
        // 退出之后重读：页上是 Sign in。
        ui.wait_for("Sign in").await;
    })
    .await;
    assert!(*account.signed_out.lock().unwrap());
}

/// 没有模型时（门道开着）⏎ 留下消息：画成你的回合，下面说 `Queued. It goes out once a model is connected.`（Puffin
/// `outgoing_entries`）；接上模型后建会话发出，agent 回答。
#[tokio::test(flavor = "multi_thread")]
async fn a_message_sent_before_a_model_waits_for_one() {
    let model = GatedModel::scripted(vec![text_response(&["answered"])]);
    let account = Arc::new(FakeAccount::default());
    let mut setup = setup(&account);
    setup.selection = Some(
        rust_cli_kernel_api::model::ModelSelection {
            model: String::new(),
            ..rust_cli_kernel_api::testing::selection()
        }
        .into(),
    );
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("Connect a model to start.").await;
        ui.submit("hello there");
        ui.wait("the queued message", |s| {
            s.contains("hello there")
                && s.contains("Queued. It goes out once a model is connected.")
        })
        .await;
        ui.key(KeyCode::Char('3'));
        ui.wait_for("Which service is the key for?").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Paste your Z.ai key below.").await;
        ui.type_text("sk-secret");
        ui.key(KeyCode::Enter);
        ui.wait_for("✓ Connected to Z.ai with your key. other-model answered in 0.0 s.")
            .await;
        let screen = ui.wait_for("answered").await;
        assert!(!screen.contains("Queued."), "{screen}");
    })
    .await;
}
