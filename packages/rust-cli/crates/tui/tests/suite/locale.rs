//! 界面语言（docs/specs/rust-cli/tui.md §7.1）：配置的 `ui.locale` 盖过 `LANG`；`/locale` 先写回、成功后切换；
//! 写失败不切换；写入在途时再输入的语言排在后面、只留最后一个。
use std::sync::{Arc, Mutex};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use rust_cli_kernel_api::testing::text_response;
use rust_cli_tui::{LocaleStore, Services, UiLocale};
use tokio::sync::oneshot;

use super::support::{GatedModel, Setup, with_ui};

const CONFIG: &str = "/Users/dev/.zcode/cli/config.json";

/// 记录每次写回；`gate` 为真时每次写回等测试放行（测在途期间的输入）。
#[derive(Default)]
struct Store {
    saved: Mutex<Vec<UiLocale>>,
    gates: Mutex<Vec<oneshot::Sender<()>>>,
    gate: bool,
    fail: Option<&'static str>,
}

impl Store {
    fn saved(&self) -> Vec<UiLocale> {
        self.saved.lock().unwrap().clone()
    }

    /// 放行最早的一次在途写回；返回有没有在途的写回。
    fn release(&self) -> bool {
        let mut gates = self.gates.lock().unwrap();
        if gates.is_empty() {
            return false;
        }
        gates.remove(0).send(()).is_ok()
    }
}

impl LocaleStore for Store {
    fn save(&self, locale: UiLocale) -> BoxFuture<'static, Result<String, String>> {
        self.saved.lock().unwrap().push(locale);
        let gate = self.gate.then(|| {
            let (sender, receiver) = oneshot::channel();
            self.gates.lock().unwrap().push(sender);
            receiver
        });
        let fail = self.fail;
        async move {
            if let Some(gate) = gate {
                let _ = gate.await;
            }
            match fail {
                Some(reason) => Err(reason.to_owned()),
                None => Ok(CONFIG.to_owned()),
            }
        }
        .boxed()
    }
}

fn setup(store: &Arc<Store>, locale: Option<UiLocale>) -> Setup {
    Setup {
        services: Services {
            locale,
            locale_store: Some(store.clone()),
            account: None,
            ..Services::default()
        },
        ..Setup::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_configured_locale_wins_over_lang() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let store = Arc::new(Store::default());
    with_ui(
        model,
        setup(&store, Some(UiLocale::ZhCn)),
        |mut ui| async move {
            ui.wait_for("变更前确认").await;
            ui.submit("/locale");
            let screen = ui.wait_for("当前语言：zh-CN").await;
            assert!(screen.contains("可选语言：auto, en-US, zh-CN"), "{screen}");
        },
    )
    .await;
    assert!(store.saved().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_writes_first_and_then_changes_the_language() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let store = Arc::new(Store::default());
    with_ui(model, setup(&store, None), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/locale fr-FR");
        ui.wait_for("Unsupported locale: fr-FR. Available locales: auto, en-US, zh-CN.")
            .await;
        ui.submit("/language zh-CN");
        let screen = ui.wait_for("界面语言已切换为 zh-CN。").await;
        assert!(screen.contains(&format!("配置文件：{CONFIG}")), "{screen}");
        assert!(screen.contains("变更前确认"), "{screen}");
    })
    .await;
    assert_eq!(store.saved(), [UiLocale::ZhCn]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_write_keeps_the_language() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let store = Arc::new(Store {
        fail: Some("Unable to write config file: /Users/dev/.zcode/cli/config.json"),
        ..Store::default()
    });
    with_ui(model, setup(&store, None), |mut ui| async move {
        ui.submit("/locale zh-CN");
        let screen = ui
            .wait_for("Unable to switch locale: Unable to write config file")
            .await;
        assert!(screen.contains("Ask before changes"), "{screen}");
    })
    .await;
}

/// 写入在途时连续输入两次：只留最后一个，在途的完成后再写；结果按输入顺序。
#[tokio::test(flavor = "multi_thread")]
async fn inputs_during_a_write_are_written_after_it_in_order() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let store = Arc::new(Store {
        gate: true,
        ..Store::default()
    });
    let check = store.clone();
    with_ui(model, setup(&store, None), |mut ui| async move {
        ui.submit("/locale zh-CN");
        ui.wait("the first write", |_| check.saved().len() == 1)
            .await;
        ui.submit("/locale en-US");
        ui.submit("/locale auto");
        // 第二、三次输入还没有写：在途的写回放行之前不发新的写入。
        ui.submit("/status");
        ui.wait_for("Status").await;
        assert_eq!(check.saved(), [UiLocale::ZhCn]);
        ui.key(crossterm::event::KeyCode::Esc);
        assert!(check.release());
        ui.wait("the queued write", |_| check.saved().len() == 2)
            .await;
        assert!(check.release());
        let screen = ui
            .wait_for("Locale set to auto; effective locale is en-US.")
            .await;
        assert!(screen.contains("Ask before changes"), "{screen}");
    })
    .await;
    assert_eq!(store.saved(), [UiLocale::ZhCn, UiLocale::Auto]);
}
