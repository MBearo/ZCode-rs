//! 一个计划的历史（platform.md §11.5）：范围、四个请求的地址与失败，和 credit-usage 的整理（数值对照 App 的 mapper）。

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;
use crate::fake_server_tests::{Captured, Harness, NOW_MS, Reply, Server, ZAI};
use crate::usage::tests::manual_login;
use rust_cli_base::account::Family;

const ACTIVITY: &str = "/api/monitor/credit-usage/activity";
const DETAIL: &str = "/api/monitor/credit-usage/usage-detail";
const HEALTH: &str = "/api/monitor/usage/model-performance-day";

fn path(request: &Captured) -> &str {
    request.path.split('?').next().unwrap_or_default()
}

fn query(request: &Captured) -> Vec<(String, String)> {
    let url = Url::parse(&format!("http://x{}", request.path)).unwrap();
    url.query_pairs().into_owned().collect()
}

fn target() -> PlanTarget {
    PlanTarget {
        provider: ZAI.to_owned(),
        family: Family::Zai,
        team: None,
    }
}

#[test]
fn ranges_follow_the_app() {
    let today = time::day_of_date("2026-10-30").unwrap();
    let day = span(HistoryRange::Today, today);
    assert_eq!(
        (day.start.as_str(), day.end.as_str()),
        ("2026-10-30 00:00:00", "2026-10-30 23:59:59")
    );
    assert!(day.hourly);
    assert_eq!(day.axis.len(), 24);
    assert_eq!(day.axis[13], "13:00:00");
    let week = span(HistoryRange::Week, today);
    assert_eq!(week.start, "2026-10-24 00:00:00");
    assert_eq!(week.axis.first().map(String::as_str), Some("2026-10-24"));
    assert_eq!(week.axis.last().map(String::as_str), Some("2026-10-30"));
    let month = span(HistoryRange::Month, today);
    assert_eq!(month.axis.len(), 30);
    assert_eq!(month.start, "2026-10-01 00:00:00");
}

#[test]
fn series_align_like_the_app() {
    let axis: Vec<String> = ["00:00:00", "01:00:00", "02:00:00"]
        .map(String::from)
        .to_vec();
    // 没有源轴且长度相同：原样。
    assert_eq!(align(&[], &axis, &[1.0, 2.0, 3.0], true), [1.0, 2.0, 3.0]);
    // 按归一化的时刻对上，缺的为 0。
    let source: Vec<String> = ["2026-10-30 02:00", "x 00:00:00"]
        .map(String::from)
        .to_vec();
    assert_eq!(align(&source, &axis, &[5.0, 7.0], true), [7.0, 0.0, 5.0]);
    assert_eq!(axis_key("2026-10-30 13:05", true), "13:05:00");
    assert_eq!(axis_key("on 2026-10-30 13:05:09", false), "2026-10-30");
    assert_eq!(axis_key("12026-10-30", false), "2026-10-30");
    assert_eq!(axis_key("week 1", false), "week 1");
}

/// credit-usage 的整理：拆分桶不当模型、合计只算模型行、工具名与次数、活动的周格子、解码速度。
#[test]
fn credit_usage_maps_like_the_app() {
    let today = time::day_of_date("2026-10-30").unwrap();
    let span = span(HistoryRange::Week, today);
    let models = json!({
        "summary": {"cacheHitRate": {"value": "0.42"}, "totalCredits": {"value": 12.5}},
        "modelUsage": {
            "xTime": ["2026-10-29", "2026-10-30"],
            "modelDataList": [
                {"modelName": "GLM-5", "sortOrder": 2, "totalTokens": 300,
                 "cachedInputTokensUsage": [10, 20], "uncachedInputTokensUsage": [1, 2], "outputTokensUsage": [3, null]},
                {"modelCode": "glm-air", "sortOrder": 1,
                 "cachedInputTokensUsage": ["5", 5], "outputTokensUsage": [0, 1]},
                {"modelCode": "cached_input", "totalTokens": 1000},
                {"modelName": " 输出 ", "totalTokens": 1000},
                {"modelName": " "}
            ]
        }
    });
    let tools = json!({"mcpUsage": {"mcpDataList": [
        {"mcpCode": "search", "mcpCallCount": [0, 2, 3]},
        {"toolName": "Reader", "totalUsageCount": "4", "sortOrder": -1},
        {}
    ]}});
    let activity = json!({
        "summary": {"totalTokens": 900, "peakDailyTokens": "400", "currentStreakDays": 2},
        "series": [
            {"date": "2026-10-30", "totalTokens": 400},
            {"date": "2026-10-20", "totalTokens": 100},
            {"date": "2026-10-29", "totalTokens": 0},
            {"date": "bad", "totalTokens": 5}
        ]
    });
    let health =
        json!({"xTime": ["2026-10-30"], "proMaxDecodeSpeed": [80, "x"], "liteDecodeSpeed": [60]});
    let snapshot = snapshot(NOW_MS, &span, [&activity, &models, &tools, &health]);
    let names: Vec<&str> = snapshot.models.iter().map(|m| m.name.as_str()).collect();
    assert_eq!(names, ["glm-air", "GLM-5"]);
    let glm = &snapshot.models[1];
    let mut cached = vec![0.0; 5];
    cached.extend([10.0, 20.0]);
    assert_eq!(glm.cached, cached);
    assert_eq!(glm.output[6], 0.0);
    assert_eq!(glm.total_tokens, 300.0);
    // 没有 totalTokens：三条之和。
    assert_eq!(snapshot.models[0].total_tokens, 11.0);
    assert_eq!(snapshot.total_tokens, 311.0);
    assert_eq!(snapshot.cache_hit_rate, Some(0.42));
    assert_eq!(snapshot.total_credits, 12.5);
    assert_eq!(
        snapshot.tools,
        [
            ToolUsage {
                name: "Reader".into(),
                calls: 4.0
            },
            ToolUsage {
                name: "search".into(),
                calls: 5.0
            },
        ]
    );
    let activity = &snapshot.activity;
    assert_eq!(
        (
            activity.total_tokens,
            activity.peak_daily_tokens,
            activity.streak_days
        ),
        (900.0, 400.0, 2.0)
    );
    // 10-20 是周二：第一周从 10-18（周日）起；10-25 那一周没有数据，不出现。
    assert_eq!(activity.start.as_deref(), Some("2026-10-18"));
    assert_eq!(
        activity.weeks,
        [
            [None, None, Some(1), None, None, None, None],
            [None, None, None, None, Some(0), Some(4), None],
        ]
    );
    assert_eq!(snapshot.health.axis, ["2026-10-30"]);
    assert_eq!(snapshot.health.pro_max, [80.0, 0.0]);
}

/// 只有拆分桶时合计用桶（App `sumCreditModelTokens`）；没有 data 的部分是空的。
#[test]
fn buckets_count_only_without_models() {
    let span = span(HistoryRange::Today, 0);
    let models = json!({"modelUsage": {"modelDataList": [
        {"modelCode": "output", "outputTokensUsage": [2, 3]},
        {"modelName": "缓存", "totalTokens": 10}
    ]}});
    let snapshot = snapshot(
        NOW_MS,
        &span,
        [&Value::Null, &models, &Value::Null, &Value::Null],
    );
    assert!(snapshot.models.is_empty());
    assert_eq!(snapshot.total_tokens, 15.0);
    assert_eq!(snapshot.activity, ActivitySnapshot::default());
    assert_eq!(snapshot.cache_hit_rate, None);
}

/// 四个请求：路径、`type`、范围与 `usageType`；头同 quota/limit。传输失败的那一块为空；信封失败整体失败，原因是 `msg`。
#[tokio::test]
async fn history_requests_follow_the_app() {
    let broken = Arc::new(Mutex::new(false));
    let script = broken.clone();
    let server = Server::start(move |request| {
        let ok = |data: Value| Reply::json(200, &json!({"code": 200, "data": data}));
        match path(request) {
            ACTIVITY => Reply::json(503, &json!({})),
            DETAIL if *script.lock().unwrap() => {
                Reply::json(200, &json!({"code": 1001, "msg": "token expired"}))
            }
            DETAIL => ok(json!({"summary": {"totalCredits": {"value": 3}}})),
            // 明确成功但没有 data：空的。
            HEALTH => Reply::json(200, &json!({"code": 200})),
            _ => Reply::json(404, &json!({})),
        }
    })
    .await;
    let harness = Harness::new(&server.url()).await;
    assert_eq!(
        harness
            .platform
            .plan_history(&target(), HistoryRange::Week)
            .await
            .unwrap_err(),
        "zai_coding_plan_api_key_required"
    );
    assert!(server.requests().is_empty());
    harness
        .write_credentials(&manual_login(ZAI, "sk-zai"))
        .await;
    let history = harness
        .platform
        .plan_history(&target(), HistoryRange::Week)
        .await
        .unwrap();
    assert_eq!(history.total_credits, 3.0);
    assert_eq!(history.activity, ActivitySnapshot::default());
    assert_eq!(history.axis.len(), 7);
    let requests = server.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests {
        assert_eq!(request.header("authorization"), Some("sk-zai"));
    }
    // 测试时钟 2023-11-14T22:13:20Z，时区 UTC。
    let activity = requests.iter().find(|r| path(r) == ACTIVITY).unwrap();
    assert_eq!(
        query(activity),
        [
            ("type".into(), "1".into()),
            ("startTime".into(), "2022-11-14 00:00:00".into()),
            ("endTime".into(), "2023-11-14 23:59:59".into()),
        ]
    );
    assert!(activity.path.contains("startTime=2022-11-14+00%3A00%3A00"));
    let mut usage: Vec<Vec<(String, String)>> = requests
        .iter()
        .filter(|r| path(r) == DETAIL)
        .map(query)
        .collect();
    usage.sort();
    assert_eq!(
        usage[0][1],
        ("startTime".into(), "2023-11-08 00:00:00".into())
    );
    assert_eq!(usage[0][3], ("usageType".into(), "MCP".into()));
    assert_eq!(usage[1][3], ("usageType".into(), "MODEL".into()));
    let health = requests.iter().find(|r| path(r) == HEALTH).unwrap();
    assert_eq!(
        query(health),
        [
            ("startTime".into(), "2023-11-08 00:00:00".into()),
            ("endTime".into(), "2023-11-14 23:59:59".into()),
        ]
    );
    *broken.lock().unwrap() = true;
    assert_eq!(
        harness
            .platform
            .plan_history(&target(), HistoryRange::Today)
            .await
            .unwrap_err(),
        "token expired"
    );
}
