use super::*;

#[test]
fn durations_use_the_compact_units() {
    assert_eq!(format_duration(0), "0s");
    assert_eq!(format_duration(59_999), "59s");
    assert_eq!(format_duration(65_000), "1m 5s");
    assert_eq!(format_duration(119_600), "2m 0s");
    assert_eq!(format_duration(3_723_000), "1h 2m 3s");
    assert_eq!(format_duration(90_061_000), "1d 1h 1m");
}

/// 去掉末尾为 0 的单位（工作线 F69：超时转后台的说明里的期限，shell-permissions.md §1.2.1）。
#[test]
fn trimmed_durations_drop_trailing_zero_units() {
    assert_eq!(format_duration_trimmed(0), "0s");
    assert_eq!(format_duration_trimmed(45_000), "45s");
    assert_eq!(format_duration_trimmed(600_000), "10m");
    assert_eq!(format_duration_trimmed(1_800_000), "30m");
    assert_eq!(format_duration_trimmed(90_000), "1m 30s");
    assert_eq!(format_duration_trimmed(3_600_000), "1h");
    assert_eq!(format_duration_trimmed(5_400_000), "1h 30m");
    assert_eq!(format_duration_trimmed(3_723_000), "1h 2m 3s");
    assert_eq!(format_duration_trimmed(7_200_000), "2h");
    assert_eq!(format_duration_trimmed(86_400_000), "1d");
    assert_eq!(format_duration_trimmed(90_000_000), "1d 1h");
    assert_eq!(format_duration_trimmed(90_061_000), "1d 1h 1m");
}

#[test]
fn numbers_keep_one_decimal_above_a_thousand() {
    assert_eq!(format_number(999), "999");
    assert_eq!(format_number(1_000), "1.0k");
    assert_eq!(format_number(1_234), "1.2k");
    assert_eq!(format_number(2_000_000), "2.0m");
}

#[test]
fn ranges_cycle_all_week_month() {
    assert_eq!(ActivityRange::All.next(), ActivityRange::Week);
    assert_eq!(ActivityRange::Week.next(), ActivityRange::Month);
    assert_eq!(ActivityRange::Month.next(), ActivityRange::All);
    assert_eq!(ActivityRange::Week.days(), Some(7));
    assert_eq!(ActivityRange::All.days(), None);
}

#[test]
fn an_empty_session_prints_the_zero_lines() {
    let text = SessionSummary::default().text();
    assert_eq!(
        text,
        "Total duration (API):  0s\nTotal duration (wall): 0s\nTotal code changes:    0 lines added, 0 lines removed\nUsage:                 0 input, 0 output, 0 cache read, 0 cache write"
    );
}

#[test]
fn models_are_right_aligned() {
    let summary = SessionSummary {
        api_duration_ms: 12_400,
        wall_duration_ms: 65_000,
        lines_added: 1,
        lines_removed: 3,
        models: vec![(
            rust_cli_base::models::GLM_5_3.to_owned(),
            Usage {
                input_tokens: 1_200,
                output_tokens: 300,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 40,
            },
        )],
    };
    assert_eq!(
        summary.text(),
        format!(
            "Total duration (API):  12s\nTotal duration (wall): 1m 5s\nTotal code changes:    1 line added, 3 lines removed\nUsage by model:\n             {}:  1.2k input, 300 output, 40 cache read, 0 cache write",
            rust_cli_base::models::GLM_5_3
        )
    );
}

#[test]
fn model_tokens_total_is_the_four_columns() {
    let tokens = ModelTokens {
        model: "m".to_owned(),
        input: 1,
        output: 2,
        cache_read: 3,
        cache_write: 4,
    };
    assert_eq!(tokens.total(), 10);
}

#[test]
fn metered_calls_round_trip_through_serde() {
    let call = Metered::Tool(ToolCall {
        tool_use: ToolUseId::new("toolu_1"),
        name: "Read".to_owned(),
        started_at_ms: 1,
        completed_at_ms: 5,
        status: CallStatus::Completed,
        output_bytes: 12,
    });
    let text = serde_json::to_string(&call).unwrap();
    assert_eq!(serde_json::from_str::<Metered>(&text).unwrap(), call);
}

/// Node `session-debug.test.ts` 的一轮：总时长 5000、首个内容 2000、输出 120 → 生成 3000 ms、40 tok/s；输入 1000、
/// 缓存读 800 → 命中率 0.8（tui-display.md §2.6，F27）。
fn node_round() -> RequestStats {
    RequestStats {
        requests: 1,
        input_tokens: 1_000,
        cache_read_tokens: 800,
        unknown_cache_usage: false,
        last: Some(LastRequest {
            output_tokens: Some(120),
            duration_ms: Some(5_000),
            first_token_ms: Some(2_000),
        }),
    }
}

#[test]
fn the_footer_stats_match_the_node_debug_panel() {
    let stats = node_round();
    assert_eq!(stats.last.unwrap().generation_ms(), Some(3_000));
    assert_eq!(stats.tokens_per_second(), Some(40.0));
    assert_eq!(stats.cache_hit_rate(), Some(0.8));
}

/// Node `does not invent TPS for invalid first content time`：首字缺失、等于或晚于结束时没有值，不用总时长代替。
#[test]
fn tps_is_absent_without_a_generation_time() {
    for first in [None, Some(5_000), Some(6_000)] {
        let mut stats = node_round();
        stats.last = Some(LastRequest {
            first_token_ms: first,
            ..stats.last.unwrap()
        });
        assert_eq!(stats.tokens_per_second(), None, "{first:?}");
    }
    let mut stats = node_round();
    stats.last = Some(LastRequest {
        duration_ms: None,
        ..stats.last.unwrap()
    });
    assert_eq!(stats.tokens_per_second(), None);
    assert_eq!(RequestStats::default().tokens_per_second(), None);
    assert_eq!(output_tps(Some(120), Some(0)), None);
}

/// 会话 actor 在内存里记的输出速度（修复线 F105，tui-display.md §2.6）：与 Node 调试面板同一个式子
/// （5000 / 2000 / 120 → 40）；首个内容缺失、等于或晚于结束时没有值，不用总时长代替；输出未知时没有值。
#[test]
fn the_output_speed_uses_the_first_content() {
    let speed = OutputSpeed {
        output_tokens: Some(120),
        duration_ms: 5_000,
        first_content_ms: Some(2_000),
    };
    assert_eq!(speed.generation_ms(), Some(3_000));
    assert_eq!(speed.tokens_per_second(), Some(40.0));
    for first in [None, Some(5_000), Some(6_000)] {
        let late = OutputSpeed {
            first_content_ms: first,
            ..speed
        };
        assert_eq!(late.tokens_per_second(), None, "{first:?}");
    }
    let unknown = OutputSpeed {
        output_tokens: None,
        ..speed
    };
    assert_eq!(unknown.tokens_per_second(), None);
    // 往返：空的项不写出。
    let json = serde_json::to_value(OutputSpeed {
        first_content_ms: None,
        ..unknown
    })
    .unwrap();
    assert_eq!(json, serde_json::json!({"duration_ms": 5_000}));
    assert_eq!(
        serde_json::from_value::<OutputSpeed>(json)
            .unwrap()
            .duration_ms,
        5_000
    );
}

/// Node `keeps zero output but never adds reasoning to the TPS numerator`：输出 0 是 0；用量未知时没有值。
#[test]
fn zero_output_is_zero_and_unknown_output_is_absent() {
    assert_eq!(output_tps(Some(0), Some(3_000)), Some(0.0));
    assert_eq!(output_tps(None, Some(3_000)), None);
}

/// 命中率是累计值；有一次未知、累计输入为 0 或没有请求时没有值（Node `hasUnknownCacheUsage`、`totalInputTokens > 0`）。
#[test]
fn the_hit_rate_needs_known_usage_and_some_input() {
    let two = RequestStats {
        requests: 2,
        input_tokens: 3_000,
        cache_read_tokens: 2_700,
        ..node_round()
    };
    assert_eq!(two.cache_hit_rate(), Some(0.9));
    let unknown = RequestStats {
        unknown_cache_usage: true,
        ..two
    };
    assert_eq!(unknown.cache_hit_rate(), None);
    let empty = RequestStats {
        input_tokens: 0,
        cache_read_tokens: 0,
        ..two
    };
    assert_eq!(empty.cache_hit_rate(), None);
    assert_eq!(RequestStats::default().cache_hit_rate(), None);
}

#[test]
fn request_stats_round_trip_and_read_old_shapes() {
    let stats = node_round();
    let text = serde_json::to_string(&stats).unwrap();
    assert_eq!(serde_json::from_str::<RequestStats>(&text).unwrap(), stats);
    // 缺省的字段可以省略。
    let bare: RequestStats =
        serde_json::from_str(r#"{"requests":0,"input_tokens":0,"cache_read_tokens":0}"#).unwrap();
    assert_eq!(bare, RequestStats::default());
}

/// 写给 Node 的用途名（工作线 F71，usage.md §2.1）：标题与 WebFetch 摘要用 Node 的名字，其余旁路照原样。
#[test]
fn query_sources_use_the_node_names() {
    use crate::model::{QuerySource, SIDE_SESSION_TITLE, SIDE_WEB_FETCH};
    let side = |name: &str| QuerySource::Side(name.to_owned());
    for (source, name) in [
        (QuerySource::MainThread, "main_turn"),
        (QuerySource::Subagent("Explore".to_owned()), "subagent"),
        (QuerySource::Compact, "compact"),
        (side(SIDE_SESSION_TITLE), "session_title"),
        (side(SIDE_WEB_FETCH), "web_fetch_processing"),
        (side("hook_agent"), "hook_agent"),
        (side("web_search_tool"), "web_search_tool"),
    ] {
        assert_eq!(source.node_name(), name, "{source:?}");
    }
    assert_eq!(SIDE_SESSION_TITLE, "generate_session_title");
    assert_eq!(SIDE_WEB_FETCH, "web_fetch_apply");
}

/// 修复线 F112（tui-panels.md §1.7）：一个完成的请求的输出速度，时刻都相对请求开始；主回合与子代理共用这一个写法
/// （各自再按来源筛）。失败与中断的请求没有。
#[test]
fn the_speed_of_a_completed_call() {
    use crate::ids::MessageId;
    use crate::model::{ModelSelection, QuerySource};
    let call = ModelCall {
        message: MessageId::new("msg_1"),
        source: QuerySource::Subagent("Explore".to_owned()),
        selection: ModelSelection {
            provider: "p".to_owned(),
            model: "m".to_owned(),
            reasoning: None,
            connection: None,
        },
        started_at_ms: 1_000,
        first_token_at_ms: Some(3_500),
        first_content_at_ms: Some(3_000),
        completed_at_ms: 6_000,
        status: CallStatus::Completed,
        stop_reason: None,
        tool_calls: 0,
        usage: Some(crate::message::Usage {
            input_tokens: 10,
            output_tokens: 120,
            cache_creation_input_tokens: 0,
            cache_read_input_tokens: 0,
        }),
        retries: 0,
        error: None,
    };
    let speed = OutputSpeed::of(&call).unwrap();
    assert_eq!(
        speed,
        OutputSpeed {
            output_tokens: Some(120),
            duration_ms: 5_000,
            first_content_ms: Some(2_000),
        }
    );
    assert_eq!(speed.tokens_per_second(), Some(40.0));
    let unknown = ModelCall {
        usage: None,
        first_content_at_ms: None,
        ..call.clone()
    };
    assert_eq!(
        OutputSpeed::of(&unknown),
        Some(OutputSpeed {
            output_tokens: None,
            duration_ms: 5_000,
            first_content_ms: None,
        })
    );
    for status in [CallStatus::Error, CallStatus::Cancelled] {
        let ended = ModelCall {
            status,
            ..call.clone()
        };
        assert_eq!(OutputSpeed::of(&ended), None, "{status:?}");
    }
}
