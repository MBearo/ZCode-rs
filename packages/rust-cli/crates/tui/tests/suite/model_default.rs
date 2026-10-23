// `/model` 与 meta+p 把模型与档位写成新工作区的默认模型（docs/specs/rust-cli/workspace-trust.md §3.2，Node
// `rememberCurrentModelSelection`）：切换被接受后写；写失败只警告；没有档位时报不完整；显式覆盖时不写。
use std::sync::{Arc, Mutex};

use crossterm::event::{KeyCode, KeyModifiers};
use futures_util::future::BoxFuture;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::{CatalogModel, ModelCatalog, ModelDirectory, WireProtocol};
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, with_ui};

/// 两个有档位的模型（Node 的执行模型都有档位）。
struct LeveledDirectory;

#[async_trait::async_trait]
impl ModelDirectory for LeveledDirectory {
    async fn catalog(&self) -> ModelCatalog {
        let model = |name: &str| CatalogModel {
            capabilities: Default::default(),
            provider: "fixture".into(),
            provider_name: "Fixture".into(),
            model: name.into(),
            protocol: WireProtocol::AnthropicMessages,
            reasoning_levels: vec!["low".into(), "high".into()],
            context_window: 200_000,
            max_output_tokens: 32_000,
            connection: None,
        };
        ModelCatalog {
            revision: 1,
            models: vec![model("scripted"), model("other-model")],
            default: None,
        }
    }
}

/// 记下写回的默认模型；`fail` 为真时写入失败。
#[derive(Default)]
struct Saved {
    models: Mutex<Vec<ModelSelection>>,
    fail: bool,
}

impl rust_cli_tui::Preferences for Saved {
    fn save_theme(
        &self,
        _theme: rust_cli_tui::UiTheme,
    ) -> BoxFuture<'static, Result<String, String>> {
        Box::pin(async { Ok(String::new()) })
    }

    fn save_default_model(
        &self,
        selection: ModelSelection,
    ) -> BoxFuture<'static, Result<String, String>> {
        self.models.lock().unwrap().push(selection);
        let result = if self.fail {
            Err("disk full".to_owned())
        } else {
            Ok("/Users/dev/.zcode/v2/provider_config.json".to_owned())
        };
        Box::pin(async move { result })
    }
}

fn setup(saved: &Arc<Saved>, models: Option<Arc<dyn ModelDirectory>>) -> Setup {
    let mut setup = Setup {
        width: 120,
        models,
        ..Setup::default()
    };
    setup.services.preferences = Some(saved.clone());
    setup
}

fn other(level: Option<&str>) -> ModelSelection {
    ModelSelection {
        provider: "fixture".into(),
        model: "other-model".into(),
        reasoning: level.map(str::to_owned),
        connection: None,
    }
}

const WARNING: &str = "Warning: The current selection is active, but could not be saved as the default for new sessions";

#[tokio::test(flavor = "multi_thread")]
async fn the_picked_model_becomes_the_default_after_the_session_accepts_it() {
    let model = GatedModel::scripted(vec![text_response(&["hi"]), text_response(&["again"])]);
    let saved = Arc::new(Saved::default());
    let directory: Arc<dyn ModelDirectory> = Arc::new(LeveledDirectory);
    let probe = saved.clone();
    with_ui(
        model.clone(),
        setup(&saved, Some(directory)),
        |mut ui| async move {
            ui.submit("hello");
            ui.wait("the first reply", |s| {
                s.contains("\u{23fa} hi") && !s.contains("esc to interrupt")
            })
            .await;
            // meta+p 打开同一个列表（tui-input.md §5）。
            ui.press(KeyCode::Char('p'), KeyModifiers::ALT);
            ui.wait_for("Select model").await;
            ui.key(KeyCode::Down);
            ui.key(KeyCode::Enter);
            ui.wait_for("Set model to other-model").await;
            ui.wait("the default write", |_| {
                !probe.models.lock().unwrap().is_empty()
            })
            .await;
            ui.submit("again please");
            ui.wait("the second turn", |s| {
                s.contains("\u{23fa} again") && !s.contains("esc to interrupt")
            })
            .await;
        },
    )
    .await;
    // 档位是列表里这个模型的最后一个（没在当前模型上调过档位）。
    assert_eq!(*saved.models.lock().unwrap(), vec![other(Some("high"))]);
    assert_eq!(model.requests()[1].selection, other(Some("high")));
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_session_the_draft_choice_is_saved_at_once() {
    let model = GatedModel::scripted(Vec::new());
    let saved = Arc::new(Saved::default());
    let directory: Arc<dyn ModelDirectory> = Arc::new(LeveledDirectory);
    let probe = saved.clone();
    with_ui(model, setup(&saved, Some(directory)), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/model fixture/other-model");
        ui.wait_for("Set model to other-model").await;
        ui.wait("the default write", |_| {
            !probe.models.lock().unwrap().is_empty()
        })
        .await;
    })
    .await;
    assert_eq!(*saved.models.lock().unwrap(), vec![other(Some("high"))]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_write_warns_and_keeps_the_switch() {
    let model = GatedModel::scripted(vec![text_response(&["done"])]);
    let saved = Arc::new(Saved {
        fail: true,
        ..Saved::default()
    });
    let directory: Arc<dyn ModelDirectory> = Arc::new(LeveledDirectory);
    with_ui(
        model.clone(),
        setup(&saved, Some(directory)),
        |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/model other-model");
            let screen = ui.wait_for(WARNING).await;
            assert!(screen.contains("disk full"), "{screen}");
            assert!(screen.contains("Set model to other-model"), "{screen}");
            ui.submit("go");
            ui.wait("the turn", |s| {
                s.contains("\u{23fa} done") && !s.contains("esc to interrupt")
            })
            .await;
        },
    )
    .await;
    assert_eq!(model.requests()[0].selection, other(Some("high")));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_selection_without_a_level_is_not_saved() {
    let model = GatedModel::scripted(Vec::new());
    let saved = Arc::new(Saved::default());
    // 缺省目录的模型没有档位：Node 报 `Current model selection is incomplete.`。
    let directory = Setup::default().models;
    let mut setup = setup(&saved, directory);
    setup.lang = "zh_CN.UTF-8";
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("变更前确认").await;
        ui.submit("/model other-model");
        let screen = ui
            .wait_for("警告：当前选择已生效，但没能保存为新会话的默认模型")
            .await;
        assert!(
            screen.contains("Current model selection is incomplete."),
            "{screen}"
        );
    })
    .await;
    assert!(saved.models.lock().unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread")]
async fn the_explicit_endpoint_does_not_write_a_default() {
    let model = GatedModel::scripted(Vec::new());
    let saved = Arc::new(Saved::default());
    with_ui(model, setup(&saved, None), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/model claude-opus-5");
        ui.wait_for("Set model to claude-opus-5").await;
    })
    .await;
    assert!(saved.models.lock().unwrap().is_empty());
}
