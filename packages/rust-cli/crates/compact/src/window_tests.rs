use rust_cli_base::models::OPUS_5_5;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::provider::TokenLimits;

use super::*;

fn selection(model: &str) -> ModelSelection {
    ModelSelection {
        provider: "p".to_owned(),
        model: model.to_owned(),
        reasoning: None,
        connection: None,
    }
}

#[test]
fn known_1m_model_compacts_at_effective_window_minus_buffer() {
    let facts = ModelFacts::resolve(&selection(OPUS_5_5), None);
    assert!(facts.recognized);
    let thresholds = Thresholds::default();
    assert_eq!(
        window(&facts, &thresholds).source,
        WindowSource::ModelDefault
    );
    // 1,000,000 − min(128000, 20000) − 13000
    assert_eq!(threshold(&facts, &thresholds), 967_000);
    assert_eq!(level(966_999, &facts, &thresholds), Level::Warn);
    assert_eq!(level(967_000, &facts, &thresholds), Level::Compact);
    assert_eq!(level(977_000, &facts, &thresholds), Level::Blocked);
    assert!(should_compact(967_000, &facts, &thresholds));
}

#[test]
fn unknown_model_uses_the_configured_window_or_the_default() {
    let configured = Some(TokenLimits {
        context_window: 128_000,
        max_output_tokens: Some(8_000),
    });
    let listed = ModelFacts::resolve(&selection("glm"), configured);
    assert_eq!(listed.context_window, 128_000);
    let thresholds = Thresholds::default();
    assert_eq!(
        window(&listed, &thresholds).source,
        WindowSource::UnknownModel
    );
    // 128000 − min(8000, 20000) − 13000
    assert_eq!(threshold(&listed, &thresholds), 107_000);
    let unlisted = ModelFacts::resolve(&selection("other"), None);
    assert_eq!(threshold(&unlisted, &thresholds), 167_000);
}

#[test]
fn known_small_window_defers_to_reactive_compaction() {
    let facts = ModelFacts {
        context_window: 200_000,
        max_output_tokens: 64_000,
        recognized: true,
    };
    let thresholds = Thresholds::default();
    assert_eq!(window(&facts, &thresholds).source, WindowSource::Auto);
    assert!(!should_compact(199_000, &facts, &thresholds));
}

#[test]
fn window_and_percent_overrides_are_bounded() {
    let facts = ModelFacts::resolve(&selection(OPUS_5_5), None);
    let windowed = Thresholds {
        window_override: Some(150_000),
        ..Thresholds::default()
    };
    assert_eq!(window(&facts, &windowed).source, WindowSource::Override);
    assert_eq!(threshold(&facts, &windowed), 117_000);
    // 下限 100k
    let tiny = Thresholds {
        window_override: Some(10),
        ..Thresholds::default()
    };
    assert_eq!(window(&facts, &tiny).window, 100_000);
    let pct = Thresholds {
        pct_override: Some(50.0),
        ..Thresholds::default()
    };
    assert_eq!(threshold(&facts, &pct), 490_000);
    let invalid = Thresholds {
        pct_override: Some(150.0),
        ..Thresholds::default()
    };
    assert_eq!(threshold(&facts, &invalid), 967_000);
}

/// `ZCODE_MAX_OUTPUT_TOKENS` 比 20000 小时摘要的预留跟着变小（修复线 F42）；比 20000 大
/// 或没设时预留仍是 20000。
#[test]
fn the_output_limit_shrinks_the_summary_reserve() {
    let facts = ModelFacts::resolve(&selection(OPUS_5_5), None);
    let limited = |limit| Thresholds {
        max_output_tokens: limit,
        ..Thresholds::default()
    };
    assert_eq!(effective(&facts, &limited(None)), 980_000);
    assert_eq!(effective(&facts, &limited(Some(64_000))), 980_000);
    assert_eq!(effective(&facts, &limited(Some(8_000))), 992_000);
    assert_eq!(threshold(&facts, &limited(Some(8_000))), 979_000);
}

/// `/context` 的缓冲（context-usage.md §4）。
#[test]
fn the_context_buffer_follows_the_window_source() {
    let facts = |window: u64, recognized: bool| ModelFacts {
        context_window: window,
        max_output_tokens: 128_000,
        recognized,
    };
    let none = Thresholds::default();
    assert_eq!(
        context_buffer(&facts(1_000_000, true), &none, true),
        Some(("Autocompact buffer", 33_000))
    );
    assert_eq!(context_buffer(&facts(200_000, true), &none, true), None);
    assert_eq!(
        context_buffer(&facts(200_000, false), &none, true),
        Some(("Autocompact buffer", 33_000))
    );
    assert_eq!(
        context_buffer(&facts(1_000_000, true), &none, false),
        Some(("Compact buffer", 3_000))
    );
}

/// 卡片认识、窗口小于 1M 的模型（来源 `auto`）：现在的卡片里没有，按事实构造。
fn small_known() -> ModelFacts {
    ModelFacts {
        context_window: 200_000,
        max_output_tokens: 64_000,
        recognized: true,
    }
}

/// F14：提示先按级别、再选文字（compact.md §2.1）。修前 TUI 只有 `N% context used` 一种，
/// 来源与开关都不看。
#[test]
fn notices_follow_the_level_the_window_source_and_the_switches() {
    let none = Thresholds::default();
    let unknown = ModelFacts::resolve(&selection("other"), None);
    let card = ModelFacts::resolve(&selection(OPUS_5_5), None);
    let used = |percent| ContextNotice::Used { percent };
    let until = |percent| ContextNotice::UntilAutoCompact { percent };
    let low = |percent, compact_hint| ContextNotice::Low {
        percent,
        compact_hint,
    };
    for (tokens, facts, thresholds, auto, command, expected) in [
        // 有效窗口 180000、阈值 167000、提示线 147000（自动压缩打开）。
        (
            146_999,
            small_known(),
            none,
            true,
            true,
            ContextNotice::Hidden,
        ),
        // 来源 `auto`：已用按有效窗口，100 − round(32999 / 180000 · 100) = 100 − 18。
        (147_001, small_known(), none, true, true, used(82)),
        (179_999, small_known(), none, true, true, used(100)),
        // 来源 `unknown-model`：剩余按阈值，round(16999 / 167000 · 100) = 10。
        (150_001, unknown, none, true, true, until(10)),
        // 过了阈值：剩余是 0（负数取 0）。
        (170_000, unknown, none, true, true, until(0)),
        // 来源 `model-default`（1M）：阈值 967000，提示线 947000。
        (946_999, card, none, true, true, ContextNotice::Hidden),
        (957_000, card, none, true, true, until(1)),
        // 窗口覆盖（来源 `env`）：有效窗口 80000、阈值 67000。
        (
            60_000,
            unknown,
            Thresholds {
                pct_override: None,
                window_override: Some(100_000),
                max_output_tokens: None,
            },
            true,
            true,
            until(10),
        ),
        // 百分比覆盖只动阈值：50% → 90000，提示线 70000，round(10000 / 90000 · 100) = 11。
        (
            80_000,
            unknown,
            Thresholds {
                pct_override: Some(50.0),
                window_override: None,
                max_output_tokens: None,
            },
            true,
            true,
            until(11),
        ),
        // 自动压缩关闭：b = 有效窗口 180000，提示线 160000；剩余 round(19999 / 180000 · 100) = 11。
        (159_999, unknown, none, false, true, ContextNotice::Hidden),
        (160_001, unknown, none, false, true, low(11, true)),
        (160_001, small_known(), none, false, true, low(11, true)),
        // `DISABLE_COMPACT`：没有“Run /compact”的后半句。
        (160_001, unknown, none, false, false, low(11, false)),
        // 阻塞线（窗口 − 预留 − 3000 = 177000）之上剩 2%。
        (177_000, unknown, none, false, true, low(2, true)),
    ] {
        assert_eq!(
            notice(tokens, &facts, &thresholds, auto, command),
            expected,
            "{tokens} {facts:?} {thresholds:?} auto={auto} command={command}"
        );
    }
}
