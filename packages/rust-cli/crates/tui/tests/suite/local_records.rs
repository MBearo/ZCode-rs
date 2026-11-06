//! 本地命令的记录（修复线 L-A，docs/specs/rust-cli/tui-manage.md §11）：`/clear`、`/cd`、`/plan`、
//! `/rename` 完成后把固定说明、命令输入与输出记进对话，下一个请求带上；还没有会话时记在新会话的草稿里、排在第一条
//! 输入之前；记录不在对话里显示。
use std::sync::Arc;

use crossterm::event::KeyCode;
use rust_cli_kernel_api::model::ModelRequest;
use rust_cli_kernel_api::testing::text_response;

use super::support::{GatedModel, Setup, request_text, with_ui};

const CAVEAT: &str = "<local-command-caveat>The command below was run directly in ZCode";

fn idle(screen: &str, text: &str) -> bool {
    screen.contains(text) && !screen.contains("esc stop")
}

/// 命令输入的那条消息（tui-manage.md §11.1）。
fn input(name: &str, args: &str) -> String {
    format!(
        "<command-name>/{name}</command-name>\n            <command-message>{name}</command-message>\n            <command-args>{args}</command-args>"
    )
}

/// 请求里的文字（JSON 转义过）是否按顺序含有这些片段。
fn in_order(request: &ModelRequest, pieces: &[String]) -> bool {
    let text = request_text(request);
    let mut from = 0;
    for piece in pieces {
        let escaped = serde_json::to_string(piece).unwrap();
        let escaped = &escaped[1..escaped.len() - 1];
        match text[from..].find(escaped) {
            Some(at) => from += at + escaped.len(),
            None => return false,
        }
    }
    true
}

/// 有档位的模型目录。
struct LeveledDirectory;

#[async_trait::async_trait]
impl rust_cli_kernel_api::provider::ModelDirectory for LeveledDirectory {
    async fn catalog(&self) -> rust_cli_kernel_api::provider::ModelCatalog {
        leveled_catalog()
    }
}

/// 第一次取目录等测试放行，之后的直接给。
struct GatedDirectory {
    first: std::sync::atomic::AtomicBool,
    gate: Arc<tokio::sync::Notify>,
}

#[async_trait::async_trait]
impl rust_cli_kernel_api::provider::ModelDirectory for GatedDirectory {
    async fn catalog(&self) -> rust_cli_kernel_api::provider::ModelCatalog {
        if self.first.swap(false, std::sync::atomic::Ordering::SeqCst) {
            self.gate.notified().await;
        }
        leveled_catalog()
    }
}

fn leveled_catalog() -> rust_cli_kernel_api::provider::ModelCatalog {
    let model = |name: &str| rust_cli_kernel_api::provider::CatalogModel {
        capabilities: Default::default(),
        provider: "fixture".into(),
        provider_name: "Fixture".into(),
        model: name.into(),
        protocol: rust_cli_kernel_api::provider::WireProtocol::AnthropicMessages,
        reasoning_levels: vec!["low".into(), "high".into()],
        context_window: 200_000,
        max_output_tokens: 32_000,
        connection: None,
    };
    rust_cli_kernel_api::provider::ModelCatalog {
        revision: 1,
        models: vec![model("scripted"), model("other-model")],
        default: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn local_commands_are_recorded_for_the_next_request() {
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["second"])]);
    let setup = Setup {
        width: 120,
        models: Some(Arc::new(LeveledDirectory)),
        ..Setup::default()
    };
    let screen = with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "   first")).await;
        // 选择器里的选择不记（tui-manage.md §11.2）；`/model 词` 2026-10-31 起只打开选择器（D26）。
        ui.pick_model("other-model").await;
        ui.wait_for("Set model to other-model").await;
        ui.submit("/plan");
        ui.wait_for("Plan · Ask before changes").await;
        ui.submit("/rename my-name");
        ui.wait_for("Session renamed to: my-name").await;
        ui.submit("next");
        ui.wait("the second reply", |s| idle(s, "   second")).await;
    })
    .await
    .unwrap_or_default();
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(!request_text(&requests[0]).contains("local-command"));
    let second = &requests[1];
    assert!(
        in_order(
            second,
            &[
                CAVEAT.to_owned(),
                input("plan", ""),
                "<local-command-stdout>Enabled plan mode</local-command-stdout>".to_owned(),
                input("rename", "my-name"),
                "<local-command-stdout>Session renamed to: my-name</local-command-stdout>".to_owned(),
                "<system-reminder>\nThe user named this session \"my-name\". This may indicate the session's focus or intent.\n</system-reminder>".to_owned(),
                "next".to_owned(),
            ]
        ),
        "{}",
        request_text(second)
    );
    // `/rename` 是 system 显示：命令输入前面没有固定说明（两条记录只有 `/plan` 带）；选择器里换模型不记。
    assert_eq!(request_text(second).matches(CAVEAT).count(), 1);
    assert!(!request_text(second).contains("<command-name>/model"));
    // 记录只给模型：界面上没有标签。
    assert!(!screen.contains("<command-name>"), "{screen}");
    assert!(!screen.contains("local-command"), "{screen}");
}

#[tokio::test(flavor = "multi_thread")]
async fn records_before_the_session_go_first_and_clear_starts_over() {
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["second"])]);
    let setup = Setup {
        width: 120,
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        // 还没有会话：记录留在新会话的草稿里。
        ui.submit("/cd");
        ui.wait_for("Usage: /cd <path>").await;
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "   first")).await;
        ui.submit("/clear");
        ui.wait("the conversation to clear", |s| !s.contains("   first"))
            .await;
        ui.submit("again");
        ui.wait("the second reply", |s| idle(s, "   second")).await;
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(
        in_order(
            &requests[0],
            &[
                CAVEAT.to_owned(),
                input("cd", ""),
                "<local-command-stdout>Usage: /cd <path></local-command-stdout>".to_owned(),
                "hello".to_owned(),
            ]
        ),
        "{}",
        request_text(&requests[0])
    );
    // `/clear` 之后的新会话：只有这次 `/clear` 的记录（之前的记录与对话一起清掉）。
    let second = request_text(&requests[1]);
    assert!(
        in_order(
            &requests[1],
            &[
                CAVEAT.to_owned(),
                input("clear", ""),
                "<local-command-stdout></local-command-stdout>".to_owned(),
                "again".to_owned(),
            ]
        ),
        "{second}"
    );
    assert!(!second.contains("Usage: /cd"), "{second}");
    assert!(!second.contains("hello"), "{second}");
}

/// 目录晚到：`/model 词` ⏎ 在目录到之前按下时列表到了才开，按当时的草稿过滤，只开一次（打出 `/model ` 已经在读目录，⏎ 不再
/// 读一次，draft_layer.rs `request_models`）；选中之后不再打开，也不记（tui-manage.md §11.2）。
#[tokio::test(flavor = "multi_thread")]
async fn a_late_catalog_opens_the_picker_once_with_the_words() {
    let model = GatedModel::scripted(vec![text_response(&["first"]), text_response(&["second"])]);
    let gate = Arc::new(tokio::sync::Notify::new());
    let setup = Setup {
        width: 120,
        models: Some(Arc::new(GatedDirectory {
            first: std::sync::atomic::AtomicBool::new(true),
            gate: gate.clone(),
        })),
        ..Setup::default()
    };
    with_ui(model.clone(), setup, |mut ui| async move {
        ui.wait_for("Ask before changes").await;
        ui.submit("hello");
        ui.wait("the first reply", |s| idle(s, "   first")).await;
        ui.submit("/model other-model");
        ui.wait_for("› /model other-model").await;
        gate.notify_one();
        ui.wait_for("←→").await;
        ui.key(KeyCode::Enter);
        ui.wait_for("Set model to other-model").await;
        ui.submit("next");
        let screen = ui.wait("the second reply", |s| idle(s, "   second")).await;
        assert!(!screen.contains("←→"), "{screen}");
    })
    .await;
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].selection.model, "other-model");
    assert_eq!(requests[1].selection.reasoning.as_deref(), Some("high"));
    assert!(!request_text(&requests[1]).contains("<command-name>/model"));
}
