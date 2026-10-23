//! footer 的请求统计（docs/specs/rust-cli/usage.md §4.1，修复线 F27）：本会话完成的主回合请求在 `model_usage` 里的
//! 聚合。每次都从行聚合，不缓存；两条查询都按 `session_id` 走 `model_usage_session_turn_idx`。
use rusqlite::{Connection, OptionalExtension};
use rust_cli_kernel_api::metering::{LastRequest, RequestStats};
use rust_cli_kernel_api::store::StoreError;

use crate::OrStorage;

/// 只算 Node 调试面板会算的行：本会话、主回合、完成（`model_request_completed`）。`+` 让来源与状态不参与选索引：
/// 没有统计信息时 SQLite 会选 `model_usage_query_source_idx`，扫遍所有会话的主回合行；按会话找只有本会话的行。
const COUNTED: &str = "session_id = ?1 and +query_source = 'main_turn' and +status = 'completed'";

/// `raw_usage_json` 里这个键是非负数（Node `token(value)` 不是 undefined）。外层先判 `json_valid`：CASE 按顺序求值，
/// 不是 JSON 的行不会走到 `json_extract`（否则整条查询报错）。
fn known(key: &str) -> String {
    format!(
        "(case when raw_usage_json is null or not json_valid(raw_usage_json) then 0
               when coalesce(json_type(raw_usage_json, '$.{key}'), '') in ('integer', 'real')
                and json_extract(raw_usage_json, '$.{key}') >= 0 then 1
               else 0 end)"
    )
}

/// Node `inputSideTokensFromStoredUsage`（`repositories/usage.ts`）：AI SDK v6 的行 `input_tokens` 已含缓存读写；
/// 旧行不含时（总数更接近“输入 + 缓存 + 输出”）加上缓存读写。
const INPUT_SIDE: &str = "case
       when input_tokens <= 0 then cache_read_input_tokens + cache_creation_input_tokens
       when cache_read_input_tokens + cache_creation_input_tokens <= 0 then input_tokens
       when coalesce(provider_total_tokens, computed_total_tokens) > 0
        and abs(coalesce(provider_total_tokens, computed_total_tokens)
                - (input_tokens + cache_read_input_tokens + cache_creation_input_tokens + output_tokens))
          < abs(coalesce(provider_total_tokens, computed_total_tokens) - (input_tokens + output_tokens))
         then input_tokens + cache_read_input_tokens + cache_creation_input_tokens
       else input_tokens
     end";

fn totals_sql() -> String {
    format!(
        "select count(*), coalesce(sum({INPUT_SIDE}), 0), coalesce(sum(cache_read_input_tokens), 0),
                coalesce(sum(1 - {input} * {read}), 0)
           from model_usage where {COUNTED}",
        input = known("inputTokens"),
        read = known("cacheReadTokens"),
    )
}

fn last_sql() -> String {
    format!(
        "select case when {output} = 1 then output_tokens end, duration_ms, time_to_first_token_ms
           from model_usage where {COUNTED}
          order by completed_at desc, started_at desc, rowid desc limit 1",
        output = known("outputTokens"),
    )
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub fn request_stats(conn: &Connection, session: &str) -> Result<RequestStats, StoreError> {
    let (requests, input, read, unknown): (i64, i64, i64, i64) = conn
        .prepare_cached(&totals_sql())
        .storage()?
        .query_row([session], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })
        .storage()?;
    let last = conn
        .prepare_cached(&last_sql())
        .storage()?
        .query_row([session], |r| {
            Ok(LastRequest {
                output_tokens: r.get::<_, Option<i64>>(0)?.map(count),
                duration_ms: r.get::<_, Option<i64>>(1)?.map(count),
                first_token_ms: r.get::<_, Option<i64>>(2)?.map(count),
            })
        })
        .optional()
        .storage()?;
    Ok(RequestStats {
        requests: count(requests),
        input_tokens: count(input),
        cache_read_tokens: count(read),
        unknown_cache_usage: unknown > 0,
        last,
    })
}

#[cfg(test)]
#[path = "usage_stats_tests.rs"]
mod tests;
