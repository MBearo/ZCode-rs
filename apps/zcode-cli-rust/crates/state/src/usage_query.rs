//! Usage aggregates on a request-scoped read-only connection (Node
//! `queryAppUsage` and `queryTaskUsage`). Spec rust-m9-usage-logs §2.4.
use crate::domain::usage::{AppRows, DAY_MS, DayModelRow, DayRow, ModelRow, TaskRow, ToolRow};
use anyhow::Result;
use rusqlite::{Connection, OpenFlags, Row, params};
use std::collections::BTreeMap;
use std::path::Path;

pub(super) fn open(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(conn)
}

fn count(row: &Row, index: usize) -> rusqlite::Result<u64> {
    Ok(row.get::<_, Option<i64>>(index)?.unwrap_or(0).max(0) as u64)
}

/// Node `queryAppUsage` over `since..=until`, days bucketed with a fixed offset.
pub(super) fn app(conn: &Connection, since: i64, until: i64, offset: i64) -> Result<AppRows> {
    let range = params![since, until];
    let mut rows = conn.query_row(
        "SELECT coalesce(sum(computed_total_tokens),0),coalesce(sum(input_tokens),0),
          coalesce(sum(output_tokens),0),coalesce(sum(reasoning_tokens),0),
          coalesce(sum(cache_creation_input_tokens),0),coalesce(sum(cache_read_input_tokens),0),count(*),
          coalesce(sum(CASE WHEN status='error' THEN 1 ELSE 0 END),0),avg(time_to_first_token_ms)
        FROM rust_model_usage WHERE started_at>=?1 AND started_at<=?2",
        range,
        |r| {
            Ok(AppRows {
                total_tokens: count(r, 0)?,
                input_tokens: count(r, 1)?,
                output_tokens: count(r, 2)?,
                reasoning_tokens: count(r, 3)?,
                cache_creation_tokens: count(r, 4)?,
                cache_read_tokens: count(r, 5)?,
                model_request_count: count(r, 6)?,
                model_error_count: count(r, 7)?,
                avg_time_to_first_token_ms: r.get(8)?,
                ..AppRows::default()
            })
        },
    )?;
    (
        rows.total_sessions,
        rows.total_turns,
        rows.avg_turn_duration_ms,
    ) = conn.query_row(
        "SELECT count(DISTINCT session_id),count(*),
          avg(CASE WHEN status='completed' THEN duration_ms ELSE NULL END)
        FROM rust_turn_usage WHERE started_at>=?1 AND started_at<=?2",
        range,
        |r| Ok((count(r, 0)?, count(r, 1)?, r.get(2)?)),
    )?;
    rows.longest_session_ms = conn.query_row(
        "SELECT coalesce(max(total),0) FROM (SELECT
          coalesce(sum(CASE WHEN status='completed' THEN duration_ms ELSE 0 END),0) AS total
        FROM rust_turn_usage WHERE started_at>=?1 AND started_at<=?2 GROUP BY session_id)",
        range,
        |r| count(r, 0),
    )?;
    (rows.tool_call_count, rows.tool_error_count) = conn.query_row(
        "SELECT count(*),coalesce(sum(CASE WHEN status='error' THEN 1 ELSE 0 END),0)
        FROM rust_tool_usage WHERE started_at>=?1 AND started_at<=?2",
        range,
        |r| Ok((count(r, 0)?, count(r, 1)?)),
    )?;
    rows.models = conn
        .prepare_cached(
            "SELECT model_id,coalesce(sum(computed_total_tokens),0) AS totalTokens,
              coalesce(sum(input_tokens),0),coalesce(sum(output_tokens),0),count(*)
            FROM rust_model_usage WHERE started_at>=?1 AND started_at<=?2
            GROUP BY model_id ORDER BY totalTokens DESC",
        )?
        .query_map(range, |r| {
            Ok(ModelRow {
                model_id: r.get(0)?,
                total_tokens: count(r, 1)?,
                input_tokens: count(r, 2)?,
                output_tokens: count(r, 3)?,
                request_count: count(r, 4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    rows.tools = conn
        .prepare_cached(
            "SELECT tool_name,count(*) AS callCount,
              coalesce(sum(CASE WHEN status='error' THEN 1 ELSE 0 END),0),avg(duration_ms)
            FROM rust_tool_usage WHERE started_at>=?1 AND started_at<=?2
            GROUP BY tool_name ORDER BY callCount DESC",
        )?
        .query_map(range, |r| {
            Ok(ToolRow {
                tool_name: r.get(0)?,
                call_count: count(r, 1)?,
                error_count: count(r, 2)?,
                avg_duration_ms: r.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let bucketed = params![offset, DAY_MS, since, until];
    // Node 先按模型行建日表，再补轮次与工具（Map 插入顺序），最后按日排序。
    let mut days: BTreeMap<i64, DayRow> = BTreeMap::new();
    for (table, column) in [
        ("rust_model_usage", "coalesce(sum(computed_total_tokens),0)"),
        ("rust_turn_usage", "count(*)"),
        ("rust_tool_usage", "count(*)"),
    ] {
        let sql = format!(
            "SELECT CAST((started_at+?1)/?2 AS INTEGER) AS dayIndex,{column} FROM {table}
            WHERE started_at>=?3 AND started_at<=?4 GROUP BY dayIndex"
        );
        let mut query = conn.prepare_cached(&sql)?;
        let found = query.query_map(bucketed, |r| Ok((r.get::<_, i64>(0)?, count(r, 1)?)))?;
        for row in found {
            let (day_index, value) = row?;
            let day = days.entry(day_index).or_insert(DayRow {
                day_index,
                ..DayRow::default()
            });
            match table {
                "rust_model_usage" => day.total_tokens = value,
                "rust_turn_usage" => day.turn_count = value,
                _ => day.tool_call_count = value,
            }
        }
    }
    rows.days = days.into_values().collect();
    rows.day_models = conn
        .prepare_cached(
            "SELECT CAST((started_at+?1)/?2 AS INTEGER) AS dayIndex,model_id,
              coalesce(sum(computed_total_tokens),0)
            FROM rust_model_usage WHERE started_at>=?3 AND started_at<=?4 GROUP BY dayIndex,model_id",
        )?
        .query_map(bucketed, |r| {
            Ok(DayModelRow {
                day_index: r.get(0)?,
                model_id: r.get(1)?,
                total_tokens: count(r, 2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}

/// The session's model requests for Node `queryTaskUsage`.
pub(super) fn task(conn: &Connection, session_id: &str) -> Result<Vec<TaskRow>> {
    let rows = conn
        .prepare_cached(
            "SELECT query_source,status,input_tokens,output_tokens,reasoning_tokens,
              cache_creation_input_tokens,cache_read_input_tokens,computed_total_tokens,provider_total_tokens
            FROM rust_model_usage WHERE session_id=?1 ORDER BY started_at ASC,id ASC",
        )?
        .query_map([session_id], |r| {
            Ok(TaskRow {
                query_source: r.get(0)?,
                status: r.get(1)?,
                input_tokens: count(r, 2)?,
                output_tokens: count(r, 3)?,
                reasoning_tokens: count(r, 4)?,
                cache_creation_tokens: count(r, 5)?,
                cache_read_tokens: count(r, 6)?,
                computed_total_tokens: count(r, 7)?,
                provider_total_tokens: r.get::<_, Option<i64>>(8)?.map(|n| n.max(0) as u64),
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(rows)
}
