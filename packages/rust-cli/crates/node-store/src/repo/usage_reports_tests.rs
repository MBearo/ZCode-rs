//! 用量报表的 SQL（usage.md §4.1、§5.2）：活动统计的消息查询只走覆盖索引，footer 的两条查询按会话找，按天分组的
//! 文字不变。取值与结果在 `usage_query_tests.rs`、`usage_stats_tests.rs`（经 `StoreQueries`）。
use super::{days_sql, last_sql, sessions_sql, totals_sql};

/// 一个空库（迁移齐全）。
fn empty() -> (tempfile::TempDir, rusqlite::Connection) {
    let dir = tempfile::tempdir().unwrap();
    let (conn, _) = super::super::open::open(
        &dir.path().join("db.sqlite"),
        super::super::open::MIGRATION_LOCK_WAIT,
        1,
    )
    .unwrap();
    (dir, conn)
}

#[test]
fn the_message_queries_only_scan_the_covering_index() {
    // 大库也要快（usage.md §5.2）：会话在外层，消息只走覆盖索引，不读消息行（行里是大 JSON）。
    let (_dir, conn) = empty();
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
fn the_request_stats_search_by_session() {
    let (_dir, conn) = empty();
    for sql in [totals_sql(), last_sql()] {
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

#[test]
fn the_day_bucket_sql_text_is_unchanged() {
    // F58：一天的毫秒数改为引用 base 的 `DAY_MS`（u64），SQL 里仍是同一段文字。
    assert!(
        days_sql().starts_with("select cast((m.time_created + ?1) / 86400000 as integer) as day")
    );
}
