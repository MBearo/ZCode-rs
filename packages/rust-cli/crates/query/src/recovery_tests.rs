use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::model::ModelError;

use super::*;

fn reply(stop: StopReason, content: Vec<ContentBlock>) -> AssistantMessage {
    AssistantMessage {
        id: MessageId::new("msg_a"),
        content,
        model: "m".to_owned(),
        stop_reason: Some(stop),
        usage: None,
    }
}

fn text(value: &str) -> ContentBlock {
    ContentBlock::Text {
        text: value.to_owned(),
    }
}

fn thinking() -> ContentBlock {
    ContentBlock::Thinking {
        thinking: "plan".to_owned(),
        signature: Some("sig".to_owned()),
    }
}

fn retried(outcome: &Outcome) -> Option<(Fate, &'static str)> {
    match outcome {
        Outcome::Retry { fate, source, .. } => Some((*fate, *source)),
        Outcome::Completed | Outcome::Failed { .. } => None,
    }
}

#[test]
fn the_output_limit_is_resumed_three_times_then_fails() {
    let mut guards = Guards::default();
    let cut = reply(StopReason::MaxTokens, vec![text("half")]);
    for _ in 0..3 {
        assert_eq!(
            retried(&guards.decide(&cut, &QuerySource::MainThread, 128_000)),
            Some((Fate::Keep, "max_output_tokens_recovery"))
        );
    }
    assert_eq!(
        guards.decide(&cut, &QuerySource::MainThread, 128_000),
        Outcome::Failed {
            // 修复线 F51b（query.md §1）：主语是 the model。
            message: "API Error: The model's response exceeded the 128000 output token maximum. To configure this behavior, set the ZCODE_MAX_OUTPUT_TOKENS environment variable.".to_owned()
        }
    );
    // 工具轮之后重新计数。
    guards.next_turn();
    assert!(retried(&guards.decide(&cut, &QuerySource::MainThread, 128_000)).is_some());
}

#[test]
fn a_tool_use_stop_without_tool_calls_is_retried_once_and_resets_the_output_count() {
    let mut guards = Guards::default();
    let cut = reply(StopReason::MaxTokens, vec![text("half")]);
    for _ in 0..3 {
        guards.decide(&cut, &QuerySource::MainThread, 1);
    }
    let leaked = reply(StopReason::ToolUse, vec![text("<invoke name=\"Read\">")]);
    assert_eq!(
        retried(&guards.decide(&leaked, &QuerySource::MainThread, 1)),
        Some((Fate::Discard, "malformed_tool_use_retry"))
    );
    // 输出上限续写次数已清零。
    assert!(retried(&guards.decide(&cut, &QuerySource::MainThread, 1)).is_some());
    assert_eq!(
        guards.decide(&leaked, &QuerySource::MainThread, 1),
        Outcome::Failed {
            message: "The model's tool call could not be parsed (retry also failed).".to_owned()
        }
    );
}

#[test]
fn a_reply_without_visible_text_is_nudged_once() {
    let mut guards = Guards::default();
    let silent = reply(StopReason::EndTurn, vec![thinking(), text("  \n")]);
    assert_eq!(
        retried(&guards.decide(&silent, &QuerySource::MainThread, 1)),
        Some((Fate::Hide, "thinking_only_retry"))
    );
    assert_eq!(
        guards.decide(&silent, &QuerySource::MainThread, 1),
        Outcome::Completed
    );
    let empty = reply(StopReason::StopSequence, Vec::new());
    assert!(retried(&Guards::default().decide(&empty, &QuerySource::MainThread, 1)).is_some());
    let spoken = reply(StopReason::EndTurn, vec![thinking(), text("done")]);
    assert_eq!(
        Guards::default().decide(&spoken, &QuerySource::MainThread, 1),
        Outcome::Completed
    );
}

#[test]
fn compaction_and_quiet_side_queries_are_not_nudged() {
    let silent = reply(StopReason::EndTurn, vec![thinking()]);
    let decide = |source: QuerySource| Guards::default().decide(&silent, &source, 1);
    assert_eq!(decide(QuerySource::Compact), Outcome::Completed);
    assert_eq!(
        decide(QuerySource::Side("away_summary".to_owned())),
        Outcome::Completed
    );
    assert!(retried(&decide(QuerySource::Side("title".to_owned()))).is_some());
    assert!(retried(&decide(QuerySource::Subagent("Explore".to_owned()))).is_some());
}

#[test]
fn other_stop_reasons_end_the_turn() {
    for stop in [
        StopReason::Refusal,
        StopReason::PauseTurn,
        StopReason::Unknown,
    ] {
        let silent = reply(stop, Vec::new());
        assert_eq!(
            Guards::default().decide(&silent, &QuerySource::MainThread, 1),
            Outcome::Completed
        );
    }
}

fn cut(content: Vec<ContentBlock>) -> AssistantMessage {
    reply(StopReason::EndTurn, content)
}

#[test]
fn a_cut_stream_is_resumed_only_for_subagents_and_print_mode() {
    let network = ModelError::Network {
        message: "reset".to_owned(),
    };
    let half = cut(vec![text("half")]);
    // 非交互的主线程：主线程的提示。
    let mut guards = Guards::default();
    let Outcome::Retry {
        fate,
        nudge,
        source,
    } = guards.truncated(&half, &QuerySource::MainThread, true, &network)
    else {
        panic!("expected a retry");
    };
    assert_eq!((fate, source), (Fate::Keep, "truncated_response_recovery"));
    assert!(nudge.starts_with("Your response above was cut off mid-stream. Resume directly"));
    // 子代理：要求整段重写。
    let subagent = QuerySource::Subagent("Explore".to_owned());
    let Outcome::Retry { nudge, .. } =
        Guards::default().truncated(&half, &subagent, false, &network)
    else {
        panic!("expected a retry");
    };
    assert!(nudge.contains("only your next message is delivered"));
    // 交互式主线程：直接以 API 错误结束，文本按错误种类。
    let failed = |error: ModelError| {
        Guards::default().truncated(&half, &QuerySource::MainThread, false, &error)
    };
    assert_eq!(
        failed(network.clone()),
        Outcome::Failed {
            message:
                "API Error: Connection lost mid-response. The response above may be incomplete."
                    .to_owned()
        }
    );
    assert_eq!(
        failed(ModelError::Server {
            status: 500,
            message: "x".to_owned()
        }),
        Outcome::Failed {
            message: "API Error: Server error mid-response. The response above may be incomplete."
                .to_owned()
        }
    );
    assert_eq!(
        failed(ModelError::Protocol {
            message: "x".to_owned()
        }),
        Outcome::Failed {
            message:
                "API Error: The response stream was malformed. The response above may be incomplete."
                    .to_owned()
        }
    );
    // 流空闲看门狗中止（L35，stall-visibility.md §2.3）。
    assert_eq!(
        failed(ModelError::StreamIdle { idle_ms: 300_000 }),
        Outcome::Failed {
            message:
                "API Error: The response stopped arriving. The response above may be incomplete."
                    .to_owned()
        }
    );
}

#[test]
fn cut_streams_share_the_output_limit_count() {
    let network = ModelError::Network {
        message: "reset".to_owned(),
    };
    let mut guards = Guards::default();
    let cut_reply = reply(StopReason::MaxTokens, vec![text("half")]);
    guards.decide(&cut_reply, &QuerySource::MainThread, 1);
    guards.decide(&cut_reply, &QuerySource::MainThread, 1);
    let half = cut(vec![text("half")]);
    assert!(retried(&guards.truncated(&half, &QuerySource::MainThread, true, &network)).is_some());
    assert!(matches!(
        guards.truncated(&half, &QuerySource::MainThread, true, &network),
        Outcome::Failed { .. }
    ));
}
