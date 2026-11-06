use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use rust_cli_kernel_api::compaction::ContextNotice;
use rust_cli_kernel_api::message::{CompactTrigger, Usage};
use rust_cli_kernel_api::orchestration::{
    AutoCompactOutcome, CompactProgress, CompactTracking, Compactor,
};
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
            at: None,
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
    let found = usage(config()).unwrap();
    let (tokens, window) = (found.used_tokens, found.max_tokens);
    assert_eq!(window, 80_000);
    assert_eq!(
        rust_cli_kernel_api::compaction::used_percent(tokens, window),
        75
    );
    // 生效的自动压缩位置是阈值（compact.md §2.3）；关掉自动压缩时没有位置。
    assert_eq!(found.compact_at, Some(67_000));
    let off = usage(CompactConfig {
        auto: false,
        ..config()
    })
    .unwrap();
    assert_eq!(
        (off.used_tokens, off.max_tokens, off.compact_at),
        (tokens, window, None)
    );
}

/// 内核读的设置只有一份（compact.md §2.3）：换掉它，同一个压缩器从下一次判定起按新的位置，不用重建。
#[test]
fn a_changed_threshold_applies_from_the_next_check() {
    let config = CompactConfig::default();
    let compaction = Compaction::new(
        ports(ScriptedRunner::new(Vec::new()), no_tools()),
        config.clone(),
    );
    // 用 1M 窗口的模型：缺省阈值在 967k，500k 的历史不压缩。
    let mut request = request(history(500_000), CompactTrigger::Auto);
    request.selection = rust_cli_kernel_api::model::ModelSelection::new(
        "anthropic",
        rust_cli_base::models::OPUS_5_5,
    );
    let (messages, model) = (&request.messages, &request.selection);
    assert!(!compaction.should_auto_compact(messages, model));
    config.at.set(Some(400_000));
    assert!(compaction.should_auto_compact(messages, model));
    assert_eq!(
        compaction
            .context_usage(messages, model)
            .unwrap()
            .compact_at,
        Some(400_000)
    );
    config.at.set(None);
    assert!(!compaction.should_auto_compact(messages, model));
    assert_eq!(
        compaction
            .context_usage(messages, model)
            .unwrap()
            .compact_at,
        Some(967_000)
    );
}

/// 记下压缩器说的进度；说开始时记下已经发了几次摘要请求。
struct Recorded {
    runner: Arc<ScriptedRunner>,
    said: Mutex<Vec<String>>,
}

#[async_trait]
impl CompactProgress for Recorded {
    async fn started(&self, trigger: CompactTrigger) {
        let sent = self.runner.requests().len();
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("started {trigger:?} after {sent} requests"));
    }

    async fn failed(&self, trigger: CompactTrigger, message: String) {
        self.said
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(format!("failed {trigger:?}: {message}"));
    }
}

async fn progress_of(
    replies: Vec<Reply>,
    history: Vec<rust_cli_kernel_api::message::Message>,
    mut tracking: CompactTracking,
) -> (AutoCompactOutcome, Vec<String>) {
    let runner = ScriptedRunner::new(replies);
    let compaction = Compaction::new(ports(runner.clone(), no_tools()), config());
    let progress = Recorded {
        runner,
        said: Mutex::default(),
    };
    let outcome = compaction
        .auto_compact_with(
            request(history, CompactTrigger::Auto),
            &mut tracking,
            CancellationToken::new(),
            &progress,
        )
        .await
        .unwrap();
    let said = progress
        .said
        .into_inner()
        .unwrap_or_else(PoisonError::into_inner);
    (outcome, said)
}

/// 缺陷修复（compact.md §4.1）：自动压缩在摘要请求之前说开始；以前主循环在摘要完成之后才说，Puffin 一直画 `Working`。
#[tokio::test]
async fn auto_compaction_says_it_started_before_the_summary_request() {
    let (outcome, said) =
        progress_of(vec![summary()], history(70_000), CompactTracking::default()).await;
    assert!(
        matches!(outcome, AutoCompactOutcome::Compacted { .. }),
        "{outcome:?}"
    );
    assert_eq!(said, ["started Auto after 0 requests"]);
}

/// 开始之后失败说失败（原因与结果里的相同）；没开始（熔断、分组不够）什么都不说。
#[tokio::test]
async fn auto_compaction_says_it_failed_only_after_it_started() {
    let (outcome, said) = progress_of(
        vec![Reply::Text("   ".to_owned())],
        history(70_000),
        CompactTracking::default(),
    )
    .await;
    let AutoCompactOutcome::Failed { message } = outcome else {
        panic!("{outcome:?}");
    };
    assert_eq!(
        said,
        [
            "started Auto after 0 requests".to_owned(),
            format!("failed Auto: {message}")
        ]
    );
    let (_, said) = progress_of(Vec::new(), one_round(70_000), CompactTracking::default()).await;
    assert!(said.is_empty(), "{said:?}");
    let open = CompactTracking {
        consecutive_failures: 3,
        ..CompactTracking::default()
    };
    let (_, said) = progress_of(Vec::new(), history(70_000), open).await;
    assert!(said.is_empty(), "{said:?}");
}
