use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::message::{CompactTrigger, Usage};
use rust_cli_kernel_api::orchestration::{AutoCompactOutcome, CompactTracking, Compactor};
use tokio_util::sync::CancellationToken;

use super::{CompactConfig, Compaction, Thresholds};
use crate::test_support::{Reply, ScriptedRunner, human, no_tools, ports, reply, request};

/// 窗口覆盖到下限 100k：有效窗口 80000，阈值 67000。
fn config() -> CompactConfig {
    CompactConfig {
        thresholds: Thresholds {
            window_override: Some(100_000),
            pct_override: None,
            max_output_tokens: None,
        },
        ..CompactConfig::default()
    }
}

fn history(input_tokens: u64) -> Vec<rust_cli_kernel_api::message::Message> {
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

fn summary() -> Reply {
    Reply::Text("<summary>S</summary>".to_owned())
}

#[tokio::test]
async fn below_the_threshold_nothing_happens() {
    let runner = ScriptedRunner::new(Vec::new());
    let compaction = Compaction::new(ports(runner.clone(), no_tools()), config());
    let mut tracking = CompactTracking::default();
    let request = request(history(60_000), CompactTrigger::Auto);
    assert!(!compaction.should_auto_compact(&request.messages, &request.selection));
    let outcome = compaction
        .auto_compact(request, &mut tracking, CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(outcome, AutoCompactOutcome::NotNeeded);
    assert!(runner.requests().is_empty());
}

#[tokio::test]
async fn over_the_threshold_compacts_and_resets_tracking() {
    let runner = ScriptedRunner::new(vec![summary()]);
    let compaction = Compaction::new(ports(runner.clone(), no_tools()), config());
    let mut tracking = CompactTracking {
        consecutive_failures: 2,
        ..CompactTracking::default()
    };
    let request = request(history(67_000), CompactTrigger::Manual);
    assert!(compaction.should_auto_compact(&request.messages, &request.selection));
    let outcome = compaction
        .auto_compact(request, &mut tracking, CancellationToken::new())
        .await
        .unwrap();
    let AutoCompactOutcome::Compacted { result } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(result.boundary.trigger, CompactTrigger::Auto);
    assert_eq!(result.boundary.pre_tokens, 67_000);
    assert_eq!(
        tracking,
        CompactTracking {
            compacted: true,
            ..CompactTracking::default()
        }
    );
}

#[tokio::test]
async fn failures_open_the_breaker_and_rapid_refills_trip() {
    let runner = ScriptedRunner::new(Vec::new());
    let compaction = Compaction::new(ports(runner.clone(), no_tools()), config());
    let mut open = CompactTracking {
        consecutive_failures: 3,
        ..CompactTracking::default()
    };
    let outcome = compaction
        .auto_compact(
            request(history(70_000), CompactTrigger::Auto),
            &mut open,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(outcome, AutoCompactOutcome::NotNeeded);
    let mut refilling = CompactTracking {
        compacted: true,
        turn_counter: 1,
        consecutive_failures: 0,
        consecutive_rapid_refills: 2,
    };
    let outcome = compaction
        .auto_compact(
            request(history(70_000), CompactTrigger::Auto),
            &mut refilling,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let AutoCompactOutcome::Thrashing { message } = outcome else {
        panic!("{outcome:?}");
    };
    assert!(
        message.starts_with(
            "Autocompact is thrashing: the context refilled to the limit within 3 turns"
        )
    );
    assert!(runner.requests().is_empty());
}

#[tokio::test]
async fn a_failed_compaction_counts_toward_the_breaker() {
    let runner = ScriptedRunner::new(vec![Reply::Text("   ".to_owned())]);
    let compaction = Compaction::new(ports(runner, no_tools()), config());
    let mut tracking = CompactTracking::default();
    let outcome = compaction
        .auto_compact(
            request(history(70_000), CompactTrigger::Auto),
            &mut tracking,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, AutoCompactOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(tracking.consecutive_failures, 1);
}

/// 只有一个 API 轮：要摘要的部分（第一组）没有 assistant。
fn one_round(input_tokens: u64) -> Vec<rust_cli_kernel_api::message::Message> {
    vec![
        human("u1", "first"),
        reply(
            "a1",
            "one",
            Some(Usage {
                input_tokens,
                ..Usage::default()
            }),
        ),
    ]
}

/// 过了阈值、没到阻塞线时不许“全部摘要”。
/// 不发摘要请求，记一次失败，照常发请求。
#[tokio::test]
async fn auto_without_an_assistant_to_summarize_bails_before_the_summary_request() {
    let runner = ScriptedRunner::new(Vec::new());
    let compaction = Compaction::new(ports(runner.clone(), no_tools()), config());
    let mut tracking = CompactTracking::default();
    let outcome = compaction
        .auto_compact(
            request(one_round(70_000), CompactTrigger::Auto),
            &mut tracking,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        outcome,
        AutoCompactOutcome::Failed {
            message: "Not enough messages to compact.".to_owned()
        }
    );
    assert!(runner.requests().is_empty());
    assert_eq!(tracking.consecutive_failures, 1);
}

/// 达到阻塞线（上下文窗口 − 预留 − 3000 = 977000）时允许“全部摘要”。
#[tokio::test]
async fn auto_at_the_blocking_limit_summarizes_everything() {
    let runner = ScriptedRunner::new(vec![summary()]);
    let compaction = Compaction::new(ports(runner.clone(), no_tools()), config());
    let mut tracking = CompactTracking::default();
    let outcome = compaction
        .auto_compact(
            request(one_round(977_000), CompactTrigger::Auto),
            &mut tracking,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(
        matches!(outcome, AutoCompactOutcome::Compacted { .. }),
        "{outcome:?}"
    );
    assert_eq!(runner.requests().len(), 1);
}

/// 手动与反应式压缩总是允许“全部摘要”。
#[tokio::test]
async fn manual_and_reactive_summarize_everything() {
    for trigger in [CompactTrigger::Manual, CompactTrigger::Reactive] {
        let runner = ScriptedRunner::new(vec![summary()]);
        let compaction = Compaction::new(ports(runner.clone(), no_tools()), config());
        compaction
            .compact(request(one_round(10), trigger), CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(runner.requests().len(), 1, "{trigger:?}");
    }
}

/// R5-8、F14：提示与自动压缩同一套 token、窗口与开关（compact.md §2.1）。窗口覆盖 100k：有效窗口 80000、
/// 阈值 67000，来源 `env`（不是 `auto`）。
#[test]
fn the_context_notice_uses_the_same_window_and_switches() {
    let request = request(history(60_000), CompactTrigger::Auto);
    let notice = |config: CompactConfig| {
        Compaction::new(ports(ScriptedRunner::new(Vec::new()), no_tools()), config)
            .context_notice(&request.messages, &request.selection)
    };
    // round(7000 / 67000 · 100) = 10。
    assert_eq!(
        notice(config()),
        Some(ContextNotice::UntilAutoCompact { percent: 10 })
    );
    // 自动压缩关闭：按有效窗口，提示线 60000，round(20000 / 80000 · 100) = 25。
    let off = CompactConfig {
        auto: false,
        ..config()
    };
    assert_eq!(
        notice(off.clone()),
        Some(ContextNotice::Low {
            percent: 25,
            compact_hint: true
        })
    );
    assert_eq!(
        notice(CompactConfig {
            compact_command: false,
            ..off
        }),
        Some(ContextNotice::Low {
            percent: 25,
            compact_hint: false
        })
    );
}

/// F27：footer 的 ctx 与提示、自动压缩同一套 token 与窗口（tui-display.md §2.6）。窗口覆盖 100k：有效窗口 80000；
/// 60000 token 占 75%。关掉自动压缩不改变用量。
#[test]
fn the_context_usage_uses_the_same_tokens_and_window() {
    let request = request(history(60_000), CompactTrigger::Auto);
    let usage = |config: CompactConfig| {
        Compaction::new(ports(ScriptedRunner::new(Vec::new()), no_tools()), config)
            .context_usage(&request.messages, &request.selection)
    };
    let (tokens, window) = usage(config()).unwrap();
    assert_eq!(window, 80_000);
    assert_eq!(
        rust_cli_kernel_api::compaction::used_percent(tokens, window),
        75
    );
    assert_eq!(
        usage(CompactConfig {
            auto: false,
            ..config()
        }),
        Some((tokens, window))
    );
}
