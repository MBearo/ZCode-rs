//! 管理命令与小项（docs/specs/rust-cli/tui-display.md §3、§4）：都在本地或经内核会话 API 完成，不发给模型
//! （`/plan 描述` 除外）。
use std::sync::{Arc, Mutex};

use crossterm::event::KeyCode;
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, request_text, with_ui};

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc to interrupt")
}

/// `/agents` 已按用户决定删除（tui-display.md §3.2，L33）：按未知命令提示，输入保留，不发给模型。
#[tokio::test(flavor = "multi_thread")]
async fn agents_is_an_unknown_command() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/agents");
        let screen = ui.wait_for("Unrecognized command '/agents'").await;
        assert!(screen.contains("❯ /agents"), "{screen}");
        assert!(!screen.contains("wizard has been removed"), "{screen}");
    })
    .await;
    assert!(model.requests().is_empty(), "no command reached the model");
}

#[tokio::test(flavor = "multi_thread")]
async fn skills_mcp_and_status_only_explain() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/skills");
        ui.wait_for("No skills found").await;
        ui.key(KeyCode::Esc);
        ui.submit("/mcp");
        ui.wait_for("No MCP servers are configured.").await;
        ui.key(KeyCode::Esc);
        ui.wait("the page to close", |s| !s.contains("MCP servers"))
            .await;
        ui.submit("/mcp reconnect all");
        ui.wait_for("No MCP servers are configured.").await;
        ui.submit("/status");
        // transcript 里已有 “No MCP servers are configured.”：等状态页自己的那一行。
        let page = ui
            .wait("the status page", |s| {
                s.lines()
                    .any(|line| line.contains("MCP servers") && line.contains("none"))
            })
            .await;
        assert!(page.contains("none"), "{page}");
        ui.key(KeyCode::Esc);
        ui.submit("/hooks");
        ui.wait_for("No hooks configured").await;
        ui.key(KeyCode::Esc);
        // `/tasks` 打开 Background 面板（与 pill 同一个，tui-panels.md §3.1）。
        ui.submit("/tasks");
        let list = ui.wait_for("No tasks currently running").await;
        assert!(list.contains("  Background"), "{list}");
    })
    .await;
    assert!(model.requests().is_empty(), "no command reached the model");
}

#[tokio::test(flavor = "multi_thread")]
async fn mode_and_effort_follow_node() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model, Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/mode nope");
        ui.wait_for("Unsupported mode: nope.").await;
        // 工作线 F76：照 App 只留三个模式，edit 不再收。
        ui.submit("/mode edit");
        ui.wait_for("Unsupported mode: edit. Available modes: build, guarded, yolo.")
            .await;
        // 修复线 F20：guarded 是真正的 guarded（App 的名称 Autonomous mode），以前当 Full access。
        ui.submit("/mode guarded");
        ui.wait_for("⏵⏵ Autonomous mode").await;
        // F26 起不带参数是选择器：副标题写当前模式（F76 起是 App 的名称）。
        ui.submit("/mode");
        ui.wait_for("Current mode: Autonomous mode.").await;
        ui.key(KeyCode::Esc);
        // 工作线 F87：切到完全访问先确认（yolo_confirm.rs）。
        ui.submit("/mode yolo");
        ui.confirm_full_access().await;
        ui.wait_for("⏵⏵ Full access").await;
        ui.submit("/effort");
        ui.wait_for("Effort level: auto").await;
        // 测试的模型目录没有档位：只有 auto，`/effort` 不弹选择器。
        ui.submit("/variant high");
        ui.wait_for("Usage: /effort [auto]").await;
        ui.submit("/effort auto");
        ui.wait_for("Effort level set to auto (this session only)")
            .await;
    })
    .await;
}

/// 修复线 F26（用户决定）：`/mode` 不带参数时弹出与 `/model` 同一种列表（上下键选、enter 确认、esc 取消、当前值
/// 打 ✔），选中后同 `/mode <名>`；带参数照旧直接生效（上一个测试）。
#[tokio::test(flavor = "multi_thread")]
async fn mode_without_arguments_opens_a_picker() {
    let model = GatedModel::scripted(Vec::new());
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/mode");
        let list = ui.wait_for("Choose a mode to switch to.").await;
        assert!(list.contains("Current mode: Ask before changes."), "{list}");
        // 工作线 F76：App 的三个模式（名称与说明）。
        for name in ["Ask before changes", "Autonomous mode", "Full access"] {
            assert!(list.contains(name), "{list}");
        }
        assert!(
            list.contains("Ask before changes ✔"),
            "the current mode is marked: {list}"
        );
        // esc 取消：列表关闭，模式不变。
        ui.key(KeyCode::Esc);
        let screen = ui
            .wait("the picker to close", |s| {
                !s.contains("Choose a mode to switch to.")
            })
            .await;
        assert!(!screen.contains("Mode switched"), "{screen}");
        // ↓ 两次（build → guarded → yolo），enter 确认。
        ui.submit("/mode");
        ui.wait_for("Choose a mode to switch to.").await;
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.confirm_full_access().await;
        let screen = ui.wait_for("Mode switched to Full access.").await;
        assert!(screen.contains("⏵⏵ Full access"), "{screen}");
        // 再打开时当前值跟着变。
        ui.submit("/mode");
        let list = ui.wait_for("Current mode: Full access.").await;
        assert!(list.contains("Full access ✔"), "{list}");
        ui.key(KeyCode::Esc);
    })
    .await;
    assert!(model.requests().is_empty(), "no command reached the model");
}

/// 有档位的模型目录（`/effort` 的选择器）。
struct LeveledDirectory;

#[async_trait::async_trait]
impl rust_cli_kernel_api::provider::ModelDirectory for LeveledDirectory {
    async fn catalog(&self) -> rust_cli_kernel_api::provider::ModelCatalog {
        rust_cli_kernel_api::provider::ModelCatalog {
            revision: 1,
            models: vec![rust_cli_kernel_api::provider::CatalogModel {
                capabilities: Default::default(),
                provider: "fixture".into(),
                provider_name: "Fixture".into(),
                model: "scripted".into(),
                protocol: rust_cli_kernel_api::provider::WireProtocol::AnthropicMessages,
                reasoning_levels: vec!["low".into(), "high".into()],
                context_window: 200_000,
                max_output_tokens: 32_000,
                connection: None,
            }],
            default: None,
        }
    }
}

/// 修复线 F26（用户决定）：`/effort` 不带参数时弹出档位列表（模型的档位与 auto，当前值打 ✔），选中后同
/// `/effort <档>`；esc 取消。中文界面同样。
#[tokio::test(flavor = "multi_thread")]
async fn effort_without_arguments_opens_a_picker() {
    let model = GatedModel::scripted(vec![text_response(&["ok"])]);
    let setup = Setup {
        width: 120,
        models: Some(Arc::new(LeveledDirectory)),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/effort");
        let list = ui.wait_for("Set effort level for model usage").await;
        assert!(list.contains("low"), "{list}");
        assert!(
            list.contains("Comprehensive implementation with extensive testing"),
            "{list}"
        );
        assert!(list.contains("auto ✔"), "no level yet is auto: {list}");
        ui.key(KeyCode::Esc);
        ui.wait("the picker to close", |s| {
            !s.contains("Set effort level for model usage")
        })
        .await;
        // 焦点在当前值（auto）；↑ 到 high，enter。
        ui.submit("/effort");
        ui.wait_for("Set effort level for model usage").await;
        ui.key(KeyCode::Up);
        ui.key(KeyCode::Enter);
        ui.wait_for("Set effort level to high (this session only)")
            .await;
        ui.submit("/effort");
        let list = ui.wait_for("Set effort level for model usage").await;
        assert!(list.contains("high ✔"), "{list}");
        ui.key(KeyCode::Esc);
        // 带参数照旧直接生效。
        ui.submit("/effort low");
        ui.wait_for("Set effort level to low (this session only)")
            .await;
        ui.submit("say ok");
        ui.wait("the reply", |s| idle(s, "⏺ ok")).await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests[0].selection.reasoning.as_deref(), Some("low"));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_pickers_speak_chinese() {
    let model = GatedModel::scripted(Vec::new());
    let setup = Setup {
        width: 120,
        lang: "zh_CN.UTF-8",
        models: Some(Arc::new(LeveledDirectory)),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("变更前确认").await;
        ui.submit("/mode");
        let list = ui.wait_for("选择要切换的模式").await;
        assert!(list.contains("当前模式：变更前确认"), "{list}");
        ui.key(KeyCode::Esc);
        ui.wait("the picker to close", |s| !s.contains("选择要切换的模式"))
            .await;
        ui.submit("/effort");
        let list = ui.wait_for("设置模型的推理档位").await;
        assert!(list.contains("这个模型的缺省档位"), "{list}");
        ui.key(KeyCode::Esc);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn copy_n_usage_and_exit_words() {
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["second"])]);
    let session = with_ui(model.clone(), Setup::default(), |mut ui| async move {
        ui.submit("one");
        ui.wait("the first reply", |s| idle(s, "⏺ first")).await;
        ui.submit("two");
        ui.wait("the second reply", |s| idle(s, "⏺ second")).await;
        ui.submit("/copy 2");
        ui.wait_for("Copied").await;
        assert_eq!(
            ui.copies.lock().unwrap().last().map(String::as_str),
            Some("first")
        );
        ui.submit("/copy 3");
        ui.wait_for("Only 2 responses available").await;
        ui.submit("/copy x");
        ui.wait_for("Usage: /copy [N]").await;
        ui.submit("/cost");
        let page = ui.wait_for("Total duration (wall):").await;
        // API 时长来自用量表（usage.md §4）：测试的时钟不走，每次请求的耗时是 0。
        assert!(page.contains("Total duration (API):  0s"), "{page}");
        assert!(
            page.contains("Total code changes:    0 lines added, 0 lines removed"),
            "{page}"
        );
        assert!(page.contains("scripted:"), "{page}");
        ui.key(KeyCode::Esc);
        ui.wait("the page to close", |s| !s.contains("Total duration"))
            .await;
        // 整行 `:q` 退出，不发给模型。
        ui.submit(":q");
        tokio::time::sleep(std::time::Duration::from_millis(300)).await;
    })
    .await;
    assert!(session.is_some());
    assert_eq!(model.requests().len(), 2, ":q was not sent to the model");
}

#[tokio::test(flavor = "multi_thread")]
async fn clear_with_a_name_renames_the_old_session() {
    let model = GatedModel::scripted(vec![text_response(&["hello"])]);
    let setup = Setup::default();
    let store = setup.store.clone();
    let old = Arc::new(Mutex::new(None));
    let seen = old.clone();
    let facts_of = store.clone();
    with_ui(model, setup, |mut ui| async move {
        ui.submit("hi there");
        ui.wait("the reply", |s| idle(s, "⏺ hello")).await;
        ui.submit("/status");
        let page = ui.wait_for("Session").await;
        *seen.lock().unwrap() = page.lines().find_map(|line| {
            line.trim()
                .strip_prefix("Session:")
                .map(|id| id.trim().to_owned())
        });
        ui.key(KeyCode::Esc);
        ui.submit("/clear first try");
        ui.wait("the cleared screen", |s| !s.contains("hi there"))
            .await;
        ui.wait_for("Started a new session.").await;
        // 改名的回执不提示：等它落库后再结束（脚本结束时事件循环退出，在途的回执会被丢掉）。
        let id = seen.lock().unwrap().clone().unwrap_or_default();
        let id = rust_cli_kernel_api::ids::SessionId::new(id);
        for _ in 0..200 {
            let renamed = facts_of
                .facts(&id)
                .iter()
                .any(|fact| matches!(fact, rust_cli_kernel_api::store::Fact::TitleChanged { .. }));
            if renamed {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    let old = old.lock().unwrap().clone().expect("the session id");
    // 被离开的会话改名（tui-display.md §4.6）。
    let facts = store.facts(&rust_cli_kernel_api::ids::SessionId::new(old));
    assert!(
        facts.iter().any(|fact| matches!(
            fact,
            rust_cli_kernel_api::store::Fact::TitleChanged { title } if title == "first try"
        )),
        "{facts:#?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plan_with_a_description_enters_plan_mode_and_sends_it() {
    let model = GatedModel::scripted(vec![
        text_response(&["hello"]),
        text_response(&["planning"]),
    ]);
    with_ui(model.clone(), Setup::default(), |mut ui| async move {
        // 有会话时：先切模式，回执到了再发输入。
        ui.submit("hi");
        ui.wait("the first reply", |s| idle(s, "⏺ hello")).await;
        ui.submit("/plan write a parser");
        let screen = ui.wait("the reply", |s| idle(s, "⏺ planning")).await;
        assert!(screen.contains("Plan mode"), "{screen}");
        // `/plan open` 见 plan_open.rs（要有能给出路径事实的执行环境）。
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(request_text(&requests[1]).contains("write a parser"));
}

/// 记下写回的偏好；`fail` 为真时写入失败（tui-manage.md §2）。
#[derive(Default)]
struct Saved {
    themes: Mutex<Vec<&'static str>>,
    models: Mutex<Vec<rust_cli_kernel_api::model::ModelSelection>>,
    fail: bool,
}

impl rust_cli_tui::Preferences for Saved {
    fn save_theme(
        &self,
        theme: rust_cli_tui::UiTheme,
    ) -> futures_util::future::BoxFuture<'static, Result<String, String>> {
        self.themes.lock().unwrap().push(theme.as_str());
        let result = if self.fail {
            Err("disk full".to_owned())
        } else {
            Ok("/Users/dev/.zcode/cli/config.json".to_owned())
        };
        Box::pin(async move { result })
    }

    fn save_default_model(
        &self,
        selection: rust_cli_kernel_api::model::ModelSelection,
    ) -> futures_util::future::BoxFuture<'static, Result<String, String>> {
        self.models.lock().unwrap().push(selection);
        let result = if self.fail {
            Err("disk full".to_owned())
        } else {
            Ok("/Users/dev/.zcode/v2/provider_config.json".to_owned())
        };
        Box::pin(async move { result })
    }
}

fn with_preferences(saved: &Arc<Saved>, theme: Option<rust_cli_tui::UiTheme>) -> Setup {
    let mut setup = Setup::default();
    setup.services.preferences = Some(saved.clone());
    setup.services.theme = theme;
    setup
}

#[tokio::test(flavor = "multi_thread")]
async fn theme_is_saved_to_the_config_then_applied() {
    let model = GatedModel::scripted(Vec::new());
    let saved = Arc::new(Saved::default());
    with_ui(model, with_preferences(&saved, None), |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/theme");
        let list = ui.wait_for("Light mode").await;
        assert!(list.contains("Auto (match terminal) ✔"), "{list}");
        assert!(
            list.contains("Choose the text style that looks best with your terminal"),
            "{list}"
        );
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("Theme set to light").await;
        ui.submit("/theme");
        let list = ui.wait_for("Dark mode").await;
        assert!(list.contains("Light mode ✔"), "{list}");
    })
    .await;
    assert_eq!(*saved.themes.lock().unwrap(), ["light"]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_theme_write_keeps_the_theme_and_the_configured_theme_is_current() {
    let model = GatedModel::scripted(Vec::new());
    let saved = Arc::new(Saved {
        fail: true,
        ..Saved::default()
    });
    let setup = with_preferences(&saved, Some(rust_cli_tui::UiTheme::Dark));
    with_ui(model, setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("/theme");
        let list = ui.wait_for("Light mode").await;
        assert!(list.contains("Dark mode ✔"), "{list}");
        ui.key(KeyCode::Down);
        ui.key(KeyCode::Enter);
        ui.wait_for("Unable to save the theme: disk full").await;
        ui.submit("/theme");
        let list = ui.wait_for("Light mode").await;
        assert!(list.contains("Dark mode ✔"), "{list}");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn effort_is_saved_as_the_default_for_new_sessions() {
    for fail in [false, true] {
        let model = GatedModel::scripted(Vec::new());
        let saved = Arc::new(Saved {
            fail,
            ..Saved::default()
        });
        with_ui(model, with_preferences(&saved, None), |mut ui| async move {
            ui.wait_for("Ask before changes").await;
            ui.submit("/effort auto");
            if fail {
                ui.wait_for("Failed to set effort level: disk full").await;
            } else {
                let screen = ui.wait_for("Effort level set to auto").await;
                assert!(!screen.contains("this session only"), "{screen}");
            }
        })
        .await;
        let models = saved.models.lock().unwrap();
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].model, "scripted");
        assert_eq!(models[0].reasoning, None);
    }
}
