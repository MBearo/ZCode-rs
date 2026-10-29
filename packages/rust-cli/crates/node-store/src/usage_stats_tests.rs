//! footer 的请求统计（usage.md §4.1，修复线 F27）：只看本会话完成的主回合请求；Node 形状的行按 Node 的口径读。
use rust_cli_kernel_api::ids::{MessageId, SessionId};
use rust_cli_kernel_api::message::Usage;
use rust_cli_kernel_api::metering::{CallStatus, LastRequest, ModelCall, RequestStats};
use rust_cli_kernel_api::model::{ModelSelection, QuerySource};
use rust_cli_kernel_api::store::{Fact, SessionStore, StoreError};

use crate::tests::{TestStore, batch, started};

fn sid(id: &str) -> SessionId {
    SessionId::new(id)
}

/// 一次请求：开始、首字、结束三个时刻；`usage` 为空是用量未知。
fn call(
    message: &str,
    source: QuerySource,
    times: (u64, Option<u64>, u64),
    status: CallStatus,
    usage: Option<Usage>,
) -> Fact {
    let (started_at_ms, first_token_at_ms, completed_at_ms) = times;
    Fact::ModelCalled {
        call: ModelCall {
            message: MessageId::new(message),
            source,
            selection: ModelSelection {
                provider: "p".into(),
                model: "m".into(),
                reasoning: None,
                connection: None,
            },
            started_at_ms,
            first_token_at_ms,
            first_content_at_ms: None,
            completed_at_ms,
            status,
            stop_reason: None,
            tool_calls: 0,
            usage,
            retries: 0,
            error: None,
        },
    }
}

fn usage(input: u64, write: u64, read: u64, output: u64) -> Option<Usage> {
    Some(Usage {
        input_tokens: input,
        output_tokens: output,
        cache_creation_input_tokens: write,
        cache_read_input_tokens: read,
    })
}

async fn commit(t: &TestStore, session: &str, facts: Vec<Fact>) {
    t.store
        .commit(&sid(session), batch(None, facts))
        .await
        .unwrap();
}

/// Node 写的一行（`recordModelUsage` 的列；`raw` 是 `raw_usage_json`）。
fn node_row(
    t: &TestStore,
    id: &str,
    columns: (i64, i64, i64),
    times: (i64, i64),
    raw: Option<&str>,
) {
    let (input, read, output) = columns;
    let (completed, ttft) = times;
    t.connection()
        .execute(
            "insert into model_usage (id, logical_request_id, session_id, query_source, provider_id,
               model_id, status, started_at, completed_at, duration_ms, time_to_first_token_ms,
               input_tokens, output_tokens, cache_read_input_tokens, computed_total_tokens, raw_usage_json)
             values (?1, ?1, 'sess_n', 'main_turn', 'p', 'm', 'completed', ?2 - 5000, ?2, 5000, ?3,
               ?4, ?5, ?6, ?4 + ?5, ?7)",
            rusqlite::params![id, completed, ttft, input, output, read, raw],
        )
        .unwrap();
}

/// 本工程写的行：Node 归一化的输入（含缓存读写）累加；失败与旁路请求不算；最近一个按完成时刻。子代理的行在子会话
/// （见 [`a_child_session_counts_its_subagent_requests`]），不进主会话的统计。
#[tokio::test]
async fn rust_rows_add_up_the_completed_main_turn_requests() {
    let t = TestStore::new();
    started(&t, "sess_a").await;
    commit(
        &t,
        "sess_a",
        vec![
            call(
                "msg_r1",
                QuerySource::MainThread,
                (1_000, Some(1_500), 3_500),
                CallStatus::Completed,
                usage(100, 5, 50, 20),
            ),
            call(
                "msg_r2",
                QuerySource::MainThread,
                (4_000, Some(4_100), 4_200),
                CallStatus::Error,
                usage(999, 0, 0, 9),
            ),
            call(
                "msg_t1",
                QuerySource::Side("session_title".into()),
                (4_000, Some(4_100), 9_900),
                CallStatus::Completed,
                usage(7, 0, 0, 7),
            ),
        ],
    )
    .await;
    commit(
        &t,
        "sess_a",
        vec![call(
            "msg_r3",
            QuerySource::MainThread,
            (5_000, Some(6_000), 9_000),
            CallStatus::Completed,
            usage(10, 0, 900, 120),
        )],
    )
    .await;
    let stats = t.store.request_stats(&sid("sess_a")).await.unwrap();
    assert_eq!(
        stats,
        RequestStats {
            requests: 2,
            input_tokens: 155 + 910,
            cache_read_tokens: 950,
            unknown_cache_usage: false,
            last: Some(LastRequest {
                output_tokens: Some(120),
                duration_ms: Some(4_000),
                first_token_ms: Some(1_000),
            }),
        }
    );
    assert_eq!(stats.tokens_per_second(), Some(40.0));
    // 用量未知（整条缺失）的完成请求：命中率从此不显示，它的输出也是未知。
    commit(
        &t,
        "sess_a",
        vec![call(
            "msg_r4",
            QuerySource::MainThread,
            (10_000, None, 12_000),
            CallStatus::Completed,
            None,
        )],
    )
    .await;
    let stats = t.store.request_stats(&sid("sess_a")).await.unwrap();
    assert!(stats.unknown_cache_usage);
    assert_eq!(stats.cache_hit_rate(), None);
    assert_eq!(stats.last.unwrap().output_tokens, None);
    assert_eq!(stats.tokens_per_second(), None);
}

/// Node 写的行：`raw_usage_json` 缺 `cacheReadTokens` 时算未知（Node `token(undefined)`）；旧行的输入不含缓存时
/// 按 `inputSideTokensFromStoredUsage` 加上；最近一行按 `completed_at`，不按插入顺序。
#[tokio::test]
async fn node_rows_follow_the_node_reading() {
    let t = TestStore::new();
    started(&t, "sess_n").await;
    let full =
        r#"{"inputTokens":1000,"outputTokens":120,"totalTokens":1120,"cacheReadTokens":800}"#;
    node_row(&t, "u_late", (1_000, 800, 120), (20_000, 2_000), Some(full));
    node_row(
        &t,
        "u_early",
        (1_000, 800, 120),
        (10_000, 4_000),
        Some(full),
    );
    let stats = t.store.request_stats(&sid("sess_n")).await.unwrap();
    assert_eq!(
        (stats.requests, stats.input_tokens, stats.cache_read_tokens),
        (2, 2_000, 1_600)
    );
    assert_eq!(stats.cache_hit_rate(), Some(0.8));
    // 最近完成的是 u_late（5000 − 2000 = 3000 ms，120 个输出 → 40 tok/s）。
    assert_eq!(stats.tokens_per_second(), Some(40.0));
    // 旧行：总数 115 = 输入 10 + 缓存读 100 + 输出 5，输入不含缓存，总输入按 110 算。
    node_row(
        &t,
        "u_legacy",
        (10, 100, 5),
        (15_000, 1_000),
        Some(r#"{"inputTokens":10,"outputTokens":5,"cacheReadTokens":100}"#),
    );
    t.connection()
        .execute(
            "update model_usage set computed_total_tokens = 115 where id = 'u_legacy'",
            [],
        )
        .unwrap();
    let stats = t.store.request_stats(&sid("sess_n")).await.unwrap();
    assert_eq!(
        (stats.input_tokens, stats.cache_read_tokens),
        (2_110, 1_700)
    );
    assert!(!stats.unknown_cache_usage);
    // provider 没报缓存读：Node 的 JSON 里没有这个键。
    node_row(
        &t,
        "u_nocache",
        (500, 0, 10),
        (16_000, 1_000),
        Some(r#"{"inputTokens":500,"outputTokens":10}"#),
    );
    let stats = t.store.request_stats(&sid("sess_n")).await.unwrap();
    assert!(stats.unknown_cache_usage);
    assert_eq!(stats.cache_hit_rate(), None);
    // 不是 JSON 的 raw 也算未知，查询不因它出错。
    node_row(&t, "u_bad", (1, 0, 1), (17_000, 1_000), Some("{not json"));
    assert!(
        t.store
            .request_stats(&sid("sess_n"))
            .await
            .unwrap()
            .unknown_cache_usage
    );
}

#[tokio::test]
async fn an_empty_session_has_no_stats_and_a_missing_one_is_not_found() {
    let t = TestStore::new();
    started(&t, "sess_e").await;
    let stats = t.store.request_stats(&sid("sess_e")).await.unwrap();
    assert_eq!(stats, RequestStats::default());
    assert_eq!(stats.tokens_per_second(), None);
    assert_eq!(stats.cache_hit_rate(), None);
    assert!(matches!(
        t.store.request_stats(&sid("sess_none")).await,
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn both_queries_search_by_session() {
    let dir = tempfile::tempdir().unwrap();
    let (conn, _) = crate::repo::open::open(
        &dir.path().join("db.sqlite"),
        crate::repo::open::MIGRATION_LOCK_WAIT,
        1,
    )
    .unwrap();
    for sql in [super::totals_sql(), super::last_sql()] {
        let mut plan = conn.prepare(&format!("explain query plan {sql}")).unwrap();
        let steps: Vec<String> = plan
            .query_map(["sess"], |row| row.get::<_, String>(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let text = steps.join("\n");
        assert!(
            text.contains("USING INDEX model_usage_session_turn_idx"),
            "{sql}\n{text}"
        );
    }
}

/// 修复线 F112（usage.md §4.1、tui-panels.md §1.7）：查看子代理时 footer 查它的子会话。子会话里完成的子代理请求算进
/// 统计，同一个子会话里失败的与旁路的不算；主会话不受子会话的行影响。修前只认 `main_turn`，子会话没有统计。
#[tokio::test]
async fn a_child_session_counts_its_subagent_requests() {
    let t = TestStore::new();
    started(&t, "sess_a").await;
    started(&t, "sess_subagent_agent_1").await;
    commit(
        &t,
        "sess_a",
        vec![call(
            "msg_r1",
            QuerySource::MainThread,
            (1_000, Some(1_500), 3_500),
            CallStatus::Completed,
            usage(100, 0, 100, 20),
        )],
    )
    .await;
    let explore = || QuerySource::Subagent("Explore".into());
    commit(
        &t,
        "sess_subagent_agent_1",
        vec![
            call(
                "msg_s1",
                explore(),
                (4_000, Some(4_100), 9_900),
                CallStatus::Completed,
                usage(10, 0, 30, 7),
            ),
            call(
                "msg_s2",
                explore(),
                (10_000, Some(10_100), 10_200),
                CallStatus::Error,
                usage(999, 0, 0, 9),
            ),
            call(
                "msg_s3",
                QuerySource::Side("web_fetch_apply".into()),
                (11_000, Some(11_100), 11_900),
                CallStatus::Completed,
                usage(50, 0, 0, 5),
            ),
        ],
    )
    .await;
    let child = t
        .store
        .request_stats(&sid("sess_subagent_agent_1"))
        .await
        .unwrap();
    assert_eq!(
        (child.requests, child.input_tokens, child.cache_read_tokens),
        (1, 40, 30)
    );
    assert_eq!(child.cache_hit_rate(), Some(0.75));
    let main = t.store.request_stats(&sid("sess_a")).await.unwrap();
    assert_eq!(
        (main.requests, main.input_tokens, main.cache_read_tokens),
        (1, 200, 100)
    );
}
