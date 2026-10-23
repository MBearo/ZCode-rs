//! `/usage` 的两个查询（usage.md §4、§5.2）：会话树的 API 时长、活动统计的聚合与范围、只扫索引。
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::Usage;
use rust_cli_kernel_api::metering::{
    ActivityDay, ActivityQuery, ActivityRange, CallStatus, ModelCall, ModelTokens,
};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::store::{Fact, SessionKind, SessionStore};

use super::{days_sql, sessions_sql};
use crate::tests::{TestStore, batch, meta, started, user};

const DAY: u64 = 86_400_000;

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

fn call(message: &str, model: &str, started: u64, duration: u64, status: CallStatus) -> Fact {
    let source = if message.starts_with("msg_sub") {
        QuerySource::Subagent("Explore".into())
    } else {
        QuerySource::MainThread
    };
    Fact::ModelCalled {
        call: ModelCall {
            message: MessageId::new(message),
            source,
            selection: ModelSelection {
                provider: "p".into(),
                model: model.into(),
                reasoning: None,
                connection: None,
            },
            started_at_ms: started,
            first_token_at_ms: None,
            completed_at_ms: started + duration,
            status,
            stop_reason: None,
            tool_calls: 0,
            usage: Some(Usage {
                input_tokens: 100,
                output_tokens: 20,
                cache_creation_input_tokens: 5,
                cache_read_input_tokens: 50,
            }),
            retries: 0,
            error: None,
        },
    }
}

/// 在 `at` 时刻提交一条人类输入（消息 id 不带时间：创建时间是提交时间）。
async fn said(t: &TestStore, session: &str, id: &str, at: u64) {
    t.source.at(at);
    t.store
        .commit(&sid(session), batch(None, vec![user(id, "hi")]))
        .await
        .unwrap();
}

async fn facts(t: &TestStore, session: &str, facts: Vec<Fact>) {
    t.store
        .commit(&sid(session), batch(None, facts))
        .await
        .unwrap();
}

async fn child(t: &TestStore, id: &str, parent: &str) {
    let mut child = meta(id, "/w", "");
    child.kind = SessionKind::Subagent;
    child.parent = Some(sid(parent));
    t.store.create(child).await.unwrap();
}

/// 两个会话、一个子代理子会话、一个已删除和一个没有消息的会话（今天是第 10 天，1970-01-11）。
async fn library() -> TestStore {
    let t = TestStore::new();
    started(&t, "sess_a").await;
    started(&t, "sess_b").await;
    started(&t, "sess_gone").await;
    started(&t, "sess_empty").await;
    child(&t, "sess_subagent_agent_1", "sess_a").await;
    said(&t, "sess_a", "msg_a1", DAY + 1_000).await;
    said(&t, "sess_a", "msg_a2", DAY + 2_000).await;
    said(&t, "sess_a", "msg_a3", 3 * DAY + 61_000).await;
    said(&t, "sess_b", "msg_b1", 9 * DAY).await;
    said(&t, "sess_b", "msg_b2", 10 * DAY + 5_000).await;
    said(&t, "sess_subagent_agent_1", "msg_c1", 9 * DAY).await;
    said(&t, "sess_gone", "msg_g1", 10 * DAY).await;
    t.connection()
        .execute(
            "update session set time_deleted = 1 where id = 'sess_gone'",
            [],
        )
        .unwrap();
    // 用量行：主线程、子代理、旁路请求与一条失败。
    facts(
        &t,
        "sess_a",
        vec![
            call("msg_r1", "opus", DAY + 1_100, 900, CallStatus::Completed),
            call("msg_r2", "opus", DAY + 2_100, 300, CallStatus::Error),
        ],
    )
    .await;
    facts(
        &t,
        "sess_subagent_agent_1",
        vec![call(
            "msg_sub1",
            "haiku",
            9 * DAY,
            500,
            CallStatus::Completed,
        )],
    )
    .await;
    facts(
        &t,
        "sess_b",
        vec![call(
            "msg_r3",
            "opus",
            9 * DAY + 100,
            700,
            CallStatus::Completed,
        )],
    )
    .await;
    t
}

fn query(range: ActivityRange, offset: i32) -> ActivityQuery {
    ActivityQuery {
        range,
        today: "1970-01-11".into(),
        utc_offset_minutes: offset,
    }
}

fn day(date: &str, messages: u64) -> ActivityDay {
    ActivityDay {
        date: date.into(),
        messages,
    }
}

#[tokio::test]
async fn api_duration_sums_completed_calls_of_the_session_and_its_subagents() {
    let t = library().await;
    let a = t.store.metering(&sid("sess_a")).await.unwrap();
    assert_eq!(a.api_duration_ms, 900 + 500);
    let b = t.store.metering(&sid("sess_b")).await.unwrap();
    assert_eq!(b.api_duration_ms, 700);
    assert!(t.store.metering(&sid("sess_none")).await.is_err());
}

#[tokio::test]
async fn all_time_activity_counts_top_level_sessions_and_their_messages() {
    let t = library().await;
    let stats = t
        .store
        .activity(query(ActivityRange::All, 0))
        .await
        .unwrap();
    assert_eq!(stats.sessions, 2);
    assert_eq!(stats.longest_session_ms, Some(2 * DAY + 60_000));
    assert_eq!(stats.first_session_ms, Some(DAY + 1_000));
    assert_eq!(stats.last_session_ms, Some(9 * DAY));
    assert_eq!(
        stats.days,
        vec![
            day("1970-01-02", 2),
            day("1970-01-04", 1),
            day("1970-01-10", 1),
            day("1970-01-11", 1),
        ]
    );
    // opus：三行主线程（失败的也算 token，统计 transcript 里的用量）；haiku：子代理。每行不含缓存的输入是
    // 155 − 55 = 100。
    assert_eq!(
        stats.models,
        vec![
            ModelTokens {
                model: "opus".into(),
                input: 300,
                output: 60,
                cache_read: 150,
                cache_write: 15
            },
            ModelTokens {
                model: "haiku".into(),
                input: 100,
                output: 20,
                cache_read: 50,
                cache_write: 5
            },
        ]
    );
}

#[tokio::test]
async fn a_week_keeps_sessions_started_and_messages_sent_in_its_days() {
    let t = library().await;
    let stats = t
        .store
        .activity(query(ActivityRange::Week, 0))
        .await
        .unwrap();
    assert_eq!(stats.sessions, 1);
    assert_eq!(stats.first_session_ms, Some(9 * DAY));
    assert_eq!(stats.days, vec![day("1970-01-10", 1), day("1970-01-11", 1)]);
    // 一样多时按模型名排（结果稳定）：haiku 在 opus 前面；opus 只剩第 9 天的一行。
    let names: Vec<_> = stats
        .models
        .iter()
        .map(|m| (m.model.as_str(), m.input))
        .collect();
    assert_eq!(names, [("haiku", 100), ("opus", 100)]);
}

#[tokio::test]
async fn local_days_follow_the_clock_offset() {
    let t = library().await;
    // UTC−1 小时：每条消息都在 UTC 的零点后一小时内，落到前一个本地日。
    let west = t
        .store
        .activity(query(ActivityRange::All, -60))
        .await
        .unwrap();
    assert_eq!(
        west.days
            .iter()
            .map(|d| d.date.as_str())
            .collect::<Vec<_>>(),
        ["1970-01-01", "1970-01-03", "1970-01-09", "1970-01-10"]
    );
}

#[tokio::test]
async fn legacy_rows_with_input_excluding_cache_keep_their_input() {
    let t = library().await;
    // Node 旧版写的行：`input_tokens` 不含缓存（总数 = 输入 + 缓存 + 输出），`inputSideTokensFromStoredUsage`
    // 判为旧行；没有缓存时输入原样。
    let conn = t.connection();
    for (id, input, read, total) in [("u1", 10, 100, 115), ("u2", 7, 0, 12)] {
        conn.execute(
            "insert into model_usage (id, logical_request_id, session_id, query_source, provider_id,
               model_id, status, started_at, input_tokens, output_tokens, cache_read_input_tokens,
               provider_total_tokens, computed_total_tokens)
             values (?1, ?1, 'sess_b', 'main_turn', 'p', 'legacy', 'completed', ?2, ?3, 5, ?4, ?5, ?5)",
            rusqlite::params![id, 9 * DAY as i64, input, read, total],
        )
        .unwrap();
    }
    conn.execute(
        "insert into model_usage (id, logical_request_id, session_id, query_source, provider_id,
           model_id, status, started_at, input_tokens, output_tokens)
         values ('t1', 't1', 'sess_b', 'session_title', 'p', 'title-model', 'completed', ?1, 9, 9)",
        [9 * DAY as i64],
    )
    .unwrap();
    let stats = t
        .store
        .activity(query(ActivityRange::All, 0))
        .await
        .unwrap();
    let legacy = stats.models.iter().find(|m| m.model == "legacy").unwrap();
    assert_eq!(
        (legacy.input, legacy.cache_read, legacy.output),
        (17, 100, 10)
    );
    assert!(stats.models.iter().all(|m| m.model != "title-model"));
}

#[test]
fn the_message_queries_only_scan_the_covering_index() {
    // 大库也要快（usage.md §5.2）：会话在外层，消息只走覆盖索引，不读消息行（行里是大 JSON）。
    let dir = tempfile::tempdir().unwrap();
    let (conn, _) = crate::repo::open::open(
        &dir.path().join("db.sqlite"),
        crate::repo::open::MIGRATION_LOCK_WAIT,
        1,
    )
    .unwrap();
    for (sql, params) in [(sessions_sql(), 1), (days_sql(), 2)] {
        let mut plan = conn.prepare(&format!("explain query plan {sql}")).unwrap();
        let steps: Vec<String> = plan
            .query_map(rusqlite::params_from_iter(vec![0_i64; params]), |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let text = steps.join("\n");
        assert!(
            text.contains("COVERING INDEX message_session_time_created_id_idx"),
            "{sql}\n{text}"
        );
        assert!(
            !text.contains("SCAN m\n") && !text.ends_with("SCAN m"),
            "{text}"
        );
    }
}

#[test]
fn dates_and_day_indexes_round_trip() {
    assert_eq!(super::day_index("1970-01-01"), Some(0));
    assert_eq!(super::day_index("2026-10-18"), Some(20_744));
    assert_eq!(super::day_index("not a date"), None);
}

#[test]
fn the_day_bucket_sql_text_is_unchanged() {
    // F58：一天的毫秒数改为引用 base 的 `DAY_MS`（u64），SQL 里仍是同一段文字。
    assert!(
        days_sql().starts_with("select cast((m.time_created + ?1) / 86400000 as integer) as day")
    );
}
