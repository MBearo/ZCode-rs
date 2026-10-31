//! 发请求前的阻塞线（compact.md §2.2）：什么时候拦、拦下时的文字。

use rust_cli_kernel_api::message::Usage;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::Compactor;

use super::text;
use crate::test_support::{ScriptedRunner, human, no_tools, ports, reply};
use crate::window::{ModelFacts, Thresholds, blocks};
use crate::{CompactConfig, Compaction};

fn facts(recognized: bool) -> ModelFacts {
    ModelFacts {
        context_window: 200_000,
        max_output_tokens: 32_000,
        recognized,
    }
}

/// 阻塞线 = 200000 − min(32000, 20000) − 3000 = 177000。
#[test]
fn the_line_is_the_window_minus_the_reserve_and_a_buffer() {
    let thresholds = Thresholds::default();
    // 不认识的模型：自动压缩打开也拦。
    assert!(!blocks(176_999, &facts(false), &thresholds, true));
    assert!(blocks(177_000, &facts(false), &thresholds, true));
    // 认识的、窗口小于 1M 的模型：自动压缩打开时交给上下文超限后的压缩，不拦；关掉时照样拦。
    assert!(!blocks(199_000, &facts(true), &thresholds, true));
    assert!(blocks(177_000, &facts(true), &thresholds, false));
    assert!(!blocks(176_999, &facts(true), &thresholds, false));
}

#[test]
fn the_text_names_the_compaction_error() {
    assert_eq!(text(None), "Prompt is too long");
    assert_eq!(
        text(Some("Not enough messages to compact.")),
        "Prompt is too long"
    );
    assert_eq!(
        text(Some("Error during compaction: API Error: 500 boom")),
        "Prompt is too long \u{b7} automatic compaction failed: API Error: 500 boom"
    );
    // 只取第一行，后面还有时接 `…`；按显示宽度截到 300 列。
    assert_eq!(
        text(Some("Error during compaction: first\nsecond")),
        "Prompt is too long \u{b7} automatic compaction failed: first\u{2026}"
    );
    let long = format!("Error during compaction: {}", "x".repeat(400));
    let found = text(Some(&long));
    let detail = found
        .strip_prefix("Prompt is too long \u{b7} automatic compaction failed: ")
        .unwrap();
    assert_eq!(detail, format!("{}\u{2026}", "x".repeat(299)));
    assert_eq!(
        text(Some("Error during compaction: ")),
        "Prompt is too long"
    );
}

fn selection(model: &str) -> ModelSelection {
    ModelSelection {
        provider: "p".to_owned(),
        model: model.to_owned(),
        reasoning: None,
        connection: None,
    }
}

fn history(input_tokens: u64) -> Vec<rust_cli_kernel_api::message::Message> {
    let usage = Usage {
        input_tokens,
        ..Usage::default()
    };
    vec![human("u1", "first"), reply("a1", "one", Some(usage))]
}

/// 压缩器按模型上限与开关判断：默认窗口 200000、不认识的模型，177000 起拦；自动压缩关掉时同一条线。
#[test]
fn the_compactor_blocks_at_the_line() {
    let runner = ScriptedRunner::new(Vec::new());
    let compaction = Compaction::new(ports(runner.clone(), no_tools()), CompactConfig::default());
    let model = selection(rust_cli_base::models::GLM_5);
    assert_eq!(
        compaction.blocking_limit(&history(176_000), &model, None),
        None
    );
    assert_eq!(
        compaction.blocking_limit(&history(178_000), &model, None),
        Some("Prompt is too long".to_owned())
    );
    let off = Compaction::new(
        ports(runner, no_tools()),
        CompactConfig {
            auto: false,
            ..CompactConfig::default()
        },
    );
    assert!(
        off.blocking_limit(&history(178_000), &model, None)
            .is_some()
    );
}
