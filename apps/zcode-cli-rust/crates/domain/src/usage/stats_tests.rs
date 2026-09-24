use super::*;
use serde_json::json;

fn rows() -> AppRows {
    AppRows {
        total_tokens: 300,
        input_tokens: 200,
        cache_read_tokens: 50,
        model_request_count: 4,
        model_error_count: 1,
        avg_time_to_first_token_ms: Some(120.0),
        tool_call_count: 3,
        tool_error_count: 1,
        models: vec![
            ModelRow {
                model_id: Some("a".into()),
                total_tokens: 225,
                request_count: 3,
                ..ModelRow::default()
            },
            ModelRow {
                model_id: None,
                total_tokens: 75,
                request_count: 1,
                ..ModelRow::default()
            },
        ],
        tools: vec![ToolRow {
            tool_name: "Bash".into(),
            call_count: 3,
            error_count: 1,
            avg_duration_ms: Some(12.5),
        }],
        days: vec![
            DayRow {
                day_index: 100,
                total_tokens: 100,
                turn_count: 1,
                tool_call_count: 0,
            },
            DayRow {
                day_index: 102,
                total_tokens: 40,
                turn_count: 2,
                tool_call_count: 1,
            },
            DayRow {
                day_index: 103,
                total_tokens: 160,
                turn_count: 1,
                tool_call_count: 2,
            },
        ],
        day_models: vec![
            DayModelRow {
                day_index: 103,
                model_id: Some("a".into()),
                total_tokens: 100,
            },
            DayModelRow {
                day_index: 103,
                model_id: None,
                total_tokens: 60,
            },
        ],
        ..AppRows::default()
    }
}

#[test]
fn snapshot_follows_node_builder() {
    let until = 103 * DAY_MS + 5_000;
    let options = SnapshotOptions {
        range: "all",
        time_zone: "UTC",
        tz_offset_ms: 0,
        generated_at: until as u64,
        since: 0,
        until,
    };
    let value = snapshot(&rows(), &options);
    let summary = &value["summary"];
    assert_eq!(summary["cacheHitRate"], json!(0.25));
    assert_eq!(summary["modelErrorRate"], json!(0.25));
    assert_eq!(
        (
            summary["activeDays"].clone(),
            summary["currentStreakDays"].clone(),
            summary["longestStreakDays"].clone()
        ),
        (json!(3), json!(2), json!(2))
    );
    assert_eq!(summary["avgTimeToFirstTokenMs"], json!(120));
    assert_eq!(
        summary["favoriteModel"],
        json!({"modelId": "a", "totalTokens": 225, "share": 0.75})
    );
    assert_eq!(value["heatmap"]["startDate"], "1970-04-11");
    assert_eq!(value["heatmap"]["endDate"], "1970-04-14");
    let week = value["heatmap"]["weeks"][0]["days"].as_array().unwrap();
    assert_eq!(week.len(), 7);
    assert_eq!(
        week.iter()
            .map(|d| d["level"].clone())
            .take(4)
            .collect::<Vec<_>>(),
        vec![json!(3), json!(0), json!(1), json!(4)]
    );
    assert!(week[4].is_null());
    assert_eq!(
        value["dailyModelUsage"][3]["models"],
        json!([{"modelId": "a", "totalTokens": 100}, {"modelId": null, "totalTokens": 60}])
    );
    assert_eq!(value["tools"][0]["errorRate"], json!(1.0 / 3.0));
    let ranged = SnapshotOptions {
        range: "7d",
        since: until - 7 * DAY_MS,
        ..options
    };
    let value = snapshot(&rows(), &ranged);
    assert_eq!(
        value["heatmap"]["weeks"].as_array().unwrap().len(),
        2,
        "8 days split by 7"
    );
}

#[test]
fn conversation_usage_counts_context_growth_per_source() {
    let row = |source: &str, input: u64, output: u64| TaskRow {
        query_source: source.into(),
        status: "completed".into(),
        input_tokens: input,
        output_tokens: output,
        computed_total_tokens: input + output,
        ..TaskRow::default()
    };
    let rows = vec![
        row("main_turn", 100, 10),
        row("main_turn", 150, 5),
        row("compact", 80, 20),
        row("main_turn", 60, 5),
    ];
    let value = task_usage("s", &rows);
    assert_eq!(value["inputTokens"], json!(100 + 50 + 80));
    assert_eq!(value["totalTokens"], json!(230 + 40));
    assert_eq!(value["inputBaselineBySource"], json!({"main_turn": 60}));
    let cached = TaskRow {
        input_tokens: 100,
        cache_read_tokens: 300,
        output_tokens: 10,
        provider_total_tokens: Some(410),
        ..row("main_turn", 0, 0)
    };
    assert_eq!(
        task_usage("s", &[cached])["inputTokens"],
        json!(400),
        "cache on top of input"
    );
}

#[test]
fn time_zones_resolve_like_intl() {
    let summer = 1_750_000_000_000; // 2025-06-15
    let winter = 1_736_000_000_000; // 2025-01-04
    assert_eq!(offset_ms("Asia/Shanghai", summer), 8 * 3_600_000);
    assert_eq!(offset_ms("asia/shanghai", summer), 8 * 3_600_000);
    assert_eq!(offset_ms("America/New_York", summer), -4 * 3_600_000);
    assert_eq!(offset_ms("America/New_York", winter), -5 * 3_600_000);
    assert_eq!(offset_ms("Asia/Calcutta", winter), 19_800_000);
    assert_eq!(offset_ms("+05:30", winter), 19_800_000);
    assert_eq!(offset_ms("-08", winter), -8 * 3_600_000);
    for unknown in ["", "Mars/Base", "+5:30", "+24:00"] {
        assert_eq!(offset_ms(unknown, winter), 0, "{unknown}");
    }
}
