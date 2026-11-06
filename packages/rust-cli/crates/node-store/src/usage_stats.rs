//! footer 的请求统计（docs/specs/rust-cli/usage.md §4.1，修复线 F27）：本会话自己的主循环完成的请求在 `model_usage` 里的
//! 聚合（主会话的主回合、子会话的子代理请求，F112）。每次都从行聚合，不缓存；两条查询（SQL 在 `repo::usage_reports`，
//! F138）都按 `session_id` 走 `model_usage_session_turn_idx`。
use rusqlite::Connection;
use rust_cli_kernel_api::metering::{LastRequest, RequestStats};
use rust_cli_kernel_api::store::StoreError;

use crate::OrStorage;
use crate::repo::usage_reports as reports;

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub fn request_stats(conn: &Connection, session: &str) -> Result<RequestStats, StoreError> {
    let totals = reports::request_totals(conn, session).storage()?;
    let last = reports::last_request(conn, session)
        .storage()?
        .map(|row| LastRequest {
            output_tokens: row.output_tokens.map(count),
            duration_ms: row.duration_ms.map(count),
            first_token_ms: row.first_token_ms.map(count),
        });
    Ok(RequestStats {
        requests: count(totals.requests),
        input_tokens: count(totals.input),
        cache_read_tokens: count(totals.cache_read),
        unknown_cache_usage: totals.unknown > 0,
        last,
        output_tokens: count(totals.output),
        measured_output_tokens: count(totals.measured_output),
        generation_ms: count(totals.generation_ms),
    })
}

#[cfg(test)]
#[path = "usage_stats_tests.rs"]
mod tests;
