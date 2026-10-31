//! 修复线 F61（compact.md §2）：窗口与输出上限只从模型上限查询取，自动压缩的同步把关、`auto_compact`、footer 的 ctx
//! 与“上下文快满”提示四处一致。
//!
//! 修前（旧接口 `directory` 上同样的用例）：glm-5.3 在目录里是 1M，`context_usage` 却是 `(300000, 180000)`、提示是
//! `0% until auto-compact`、同步把关放行，`auto_compact` 再按目录判定不压缩——四处互相矛盾。

use std::collections::HashMap;
use std::sync::Arc;

use rust_cli_base::models::{GLM_4_5, GLM_5_3, OPUS_5_5};
use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::message::{CompactTrigger, Message, Usage};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::orchestration::{AutoCompactOutcome, CompactTracking, Compactor};
use rust_cli_kernel_api::provider::{ModelLimits, TokenLimits};
use tokio_util::sync::CancellationToken;

use super::{CompactConfig, Compaction, Thresholds};
use crate::test_support::{Reply, ScriptedRunner, human, no_tools, ports, reply, request};

/// 按模型 ID 答的查询（没有的模型答不出）。
struct Limits(HashMap<String, TokenLimits>);

impl ModelLimits for Limits {
    fn limits(&self, selection: &ModelSelection) -> Option<TokenLimits> {
        self.0.get(selection.model.as_str()).copied()
    }
}

fn limits(entries: &[(&str, u64, Option<u64>)]) -> Option<Arc<dyn ModelLimits>> {
    let map = entries
        .iter()
        .map(|(model, context_window, max_output_tokens)| {
            (
                (*model).to_owned(),
                TokenLimits {
                    context_window: *context_window,
                    max_output_tokens: *max_output_tokens,
                },
            )
        })
        .collect();
    Some(Arc::new(Limits(map)))
}

/// 内置配置里的 glm-5.3：`contextWindow: 1000000`、`maxOutputTokens.max: 128000`（没有卡片）。
fn glm() -> Option<Arc<dyn ModelLimits>> {
    limits(&[(GLM_5_3, 1_000_000, Some(128_000))])
}

fn history(input_tokens: u64) -> Vec<Message> {
    vec![
        human("u1", "first"),
        reply("a1", "one", None),
        human("u2", "second"),
        reply(
            "a2",
            "two",
            Some(Usage {
                input_tokens,
                ..Usage::default()
            }),
        ),
    ]
}

fn selection(model: &str) -> ModelSelection {
    ModelSelection {
        provider: "p".to_owned(),
        model: model.to_owned(),
        reasoning: None,
        connection: None,
    }
}

fn compaction(runner: Arc<ScriptedRunner>, config: CompactConfig) -> Compaction {
    Compaction::new(ports(runner, no_tools()), config)
}

/// 用户报的场景：glm-5.3 在 300k token 时不压缩；footer 的有效窗口 1,000,000 − min(128000, 20000)；不提示“快满”。
#[tokio::test]
async fn a_configured_1m_window_holds_at_300k_in_all_four_checks() {
    let runner = ScriptedRunner::new(Vec::new());
    let compaction = compaction(
        runner.clone(),
        CompactConfig {
            limits: glm(),
            ..CompactConfig::default()
        },
    );
    let mut request = request(history(300_000), CompactTrigger::Auto);
    request.selection = selection(GLM_5_3);
    let (messages, model) = (&request.messages, &request.selection);
    assert!(!compaction.should_auto_compact(messages, model));
    assert_eq!(
        compaction.context_usage(messages, model),
        Some((300_000, 980_000))
    );
    assert_eq!(
        compaction.context_notice(messages, model),
        Some(ContextNotice::Hidden)
    );
    let outcome = compaction
        .auto_compact(
            request,
            &mut CompactTracking::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, AutoCompactOutcome::NotNeeded);
    assert!(runner.requests().is_empty());
}

/// 配置窗口小于默认窗口时，同步把关与 `auto_compact` 都按配置：131072 − 20000 − 13000 = 98072 起压缩（修前把关按
/// 200k，不放行）。
#[tokio::test]
async fn a_configured_small_window_compacts_at_its_own_threshold() {
    let runner = ScriptedRunner::new(vec![Reply::Text("<summary>S</summary>".to_owned())]);
    let compaction = compaction(
        runner.clone(),
        CompactConfig {
            limits: limits(&[(GLM_4_5, 131_072, Some(98_304))]),
            ..CompactConfig::default()
        },
    );
    let mut request = request(history(100_000), CompactTrigger::Auto);
    request.selection = selection(GLM_4_5);
    assert!(compaction.should_auto_compact(&request.messages, &request.selection));
    assert_eq!(
        compaction.context_usage(&request.messages, &request.selection),
        Some((100_000, 111_072))
    );
    let outcome = compaction
        .auto_compact(
            request,
            &mut CompactTracking::default(),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, AutoCompactOutcome::Compacted { .. }),
        "{outcome:?}"
    );
}

/// 配置的 `maxOutputTokens.max` 进摘要预留：min(8000, 20000) = 8000，有效窗口 1,000,000 − 8000。
#[test]
fn the_configured_output_limit_sets_the_summary_reserve() {
    let compaction = compaction(
        ScriptedRunner::new(Vec::new()),
        CompactConfig {
            limits: limits(&[("small-output", 1_000_000, Some(8_000))]),
            ..CompactConfig::default()
        },
    );
    let usage = compaction.context_usage(&history(10), &selection("small-output"));
    assert_eq!(usage, Some((10, 992_000)));
}

/// 回归：查询答不出时按卡片（有卡片的模型的卡片自己的上限：1M、输出 128000，只有 Anthropic 的卡片模型有），什么都没有时 200,000；查询没给输出上限时按卡片，
/// 再没有时不限（预留取 20000）。
#[test]
fn without_a_configured_entry_the_card_and_the_default_apply() {
    let card_1m = format!("{OPUS_5_5}[1m]");
    let config = CompactConfig {
        limits: limits(&[
            (card_1m.as_str(), 500_000, None),
            ("open-model", 300_000, None),
        ]),
        ..CompactConfig::default()
    };
    let card = config.facts(&selection(OPUS_5_5));
    assert_eq!(
        (card.context_window, card.max_output_tokens),
        (1_000_000, 128_000)
    );
    assert!(card.recognized);
    let unknown = config.facts(&selection("other"));
    assert_eq!(
        (unknown.context_window, unknown.max_output_tokens),
        (200_000, u64::from(u32::MAX))
    );
    assert!(!unknown.recognized);
    // 查询给了窗口、没给输出上限：输出上限按卡片。
    let partial = config.facts(&selection(&card_1m));
    assert_eq!(
        (partial.context_window, partial.max_output_tokens),
        (500_000, 128_000)
    );
    let open = config.facts(&selection("open-model"));
    assert_eq!(
        (open.context_window, open.max_output_tokens),
        (300_000, u64::from(u32::MAX))
    );
    // 没有查询（测试替身）：同样按卡片与默认窗口。
    let none = CompactConfig::default();
    assert_eq!(none.facts(&selection(OPUS_5_5)), card);
    assert_eq!(none.facts(&selection("other")), unknown);
}

/// 回归：`ZCODE_AUTO_COMPACT_WINDOW` 照旧盖过配置的窗口（`min(窗口, 覆盖值)`）：300000 − 20000。
#[test]
fn the_window_override_still_caps_the_configured_window() {
    let compaction = compaction(
        ScriptedRunner::new(Vec::new()),
        CompactConfig {
            limits: glm(),
            thresholds: Thresholds {
                window_override: Some(300_000),
                ..Thresholds::default()
            },
            ..CompactConfig::default()
        },
    );
    let usage = compaction.context_usage(&history(10), &selection(GLM_5_3));
    assert_eq!(usage, Some((10, 280_000)));
}
