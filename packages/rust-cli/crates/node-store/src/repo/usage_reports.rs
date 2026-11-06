//! 用量的只读报表（docs/specs/rust-cli/usage.md §4、§4.1、§5.2）：会话树的 API 时长、活动统计的三条聚合、footer 的
//! 请求统计。都是聚合 SQL，不把消息或用量行整表读进内存。
//!
//! 只有 SQL 与原始的行（SQLite 的整数）；换成 kernel-api 的 metering 类型在 `crate::usage_query` 与
//! `crate::usage_stats`（架构整理 F138：以前这里的 SQL 写在那两个文件里，审查 X5 的剩余）。
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension};

/// 一天的毫秒数：只定义在 base 的 `js::date::DAY_MS`（u64，F58）。SQL 文本与 i64 的日界算术用这个 i64 值，文本逐字不变。
pub const DAY_MS: i64 = rust_cli_base::js::date::DAY_MS as i64;

/// 会话及其子代理子会话（`parent_id` 递归）里成功请求的耗时之和（`Total duration (API)`）。
const METERING: &str = "with recursive tree(id) as (
       select ?1
       union
       select s.id from session s join tree t on s.parent_id = t.id
        where s.task_type = 'subagent_child'
     )
     select coalesce(sum(duration_ms), 0) from model_usage
      where session_id in (select id from tree) and status = 'completed'";

/// 会话树里成功请求的 `duration_ms` 之和。
pub fn api_duration(conn: &Connection, session: &str) -> Result<i64> {
    Ok(conn
        .prepare_cached(METERING)?
        .query_row([session], |r| r.get(0))?)
}

/// 统计里的会话：未删除、不是子代理或工作流的子运行（子代理的 transcript 不统计）。
const COUNTED_SESSIONS: &str = "s.time_deleted is null
       and s.task_type not in ('subagent_child', 'workflow_child', 'nested_workflow_child')";

/// 范围内开始的会话：个数、最长时长、最早与最晚的开始。开始与结束是这个会话消息的最早与最晚
/// 创建时间（每个会话两次索引上的 min / max）。
fn sessions_sql() -> String {
    format!(
        "with started as (
           select (select min(m.time_created) from message m where m.session_id = s.id) as first,
                  (select max(m.time_created) from message m where m.session_id = s.id) as last
             from session s where {COUNTED_SESSIONS}
         )
         select count(*), max(last - first), min(first), max(first)
           from started where first is not null and first >= ?1"
    )
}

/// 按本地日的消息数（`dailyActivity.messageCount`）。`cross join` 固定会话在外层：每个会话一段覆盖索引。
fn days_sql() -> String {
    format!(
        "select cast((m.time_created + ?1) / {DAY_MS} as integer) as day, count(*)
           from session s cross join message m on m.session_id = s.id
          where {COUNTED_SESSIONS} and m.time_created >= ?2
          group by day order by day"
    )
}

/// 按模型的 token（`modelUsage`）：transcript 里的请求（主线程、子代理、工作流子运行）。`input_tokens` 是
/// AI SDK 的总输入；不含缓存的输入按 Node `inputSideTokensFromStoredUsage` 的判别得出（旧行的输入不含缓存）。
const MODELS: &str = "select model_id,
       coalesce(sum(case
         when input_tokens <= 0 then 0
         when cache_read_input_tokens + cache_creation_input_tokens <= 0 then input_tokens
         when coalesce(provider_total_tokens, computed_total_tokens) > 0
          and abs(coalesce(provider_total_tokens, computed_total_tokens)
                  - (input_tokens + cache_read_input_tokens + cache_creation_input_tokens + output_tokens))
            < abs(coalesce(provider_total_tokens, computed_total_tokens) - (input_tokens + output_tokens))
           then input_tokens
         else max(input_tokens - cache_read_input_tokens - cache_creation_input_tokens, 0)
       end), 0),
       coalesce(sum(output_tokens), 0),
       coalesce(sum(cache_read_input_tokens), 0),
       coalesce(sum(cache_creation_input_tokens), 0)
     from model_usage
    where query_source in ('main_turn', 'subagent', 'workflow_child') and started_at >= ?1
    group by model_id";

/// `from`（UTC 毫秒）之后开始的会话（[`sessions_sql`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Started {
    pub count: i64,
    pub longest: Option<i64>,
    pub first: Option<i64>,
    pub last: Option<i64>,
}

pub fn started(conn: &Connection, from: i64) -> Result<Started> {
    Ok(conn
        .prepare_cached(&sessions_sql())?
        .query_row([from], |r| {
            Ok(Started {
                count: r.get(0)?,
                longest: r.get(1)?,
                first: r.get(2)?,
                last: r.get(3)?,
            })
        })?)
}

/// `from` 之后每个本地日（时区偏移 `offset` 毫秒）的消息数：（自 1970-01-01 的本地日序号，条数），按日排序。
pub fn days(conn: &Connection, offset: i64, from: i64) -> Result<Vec<(i64, i64)>> {
    Ok(conn
        .prepare_cached(&days_sql())?
        .query_map([offset, from], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?)
}

/// 一个模型在范围内的 token（[`MODELS`]）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelTotals {
    pub model: String,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
}

pub fn models(conn: &Connection, from: i64) -> Result<Vec<ModelTotals>> {
    Ok(conn
        .prepare_cached(MODELS)?
        .query_map([from], |r| {
            Ok(ModelTotals {
                model: r.get(0)?,
                input: r.get(1)?,
                output: r.get(2)?,
                cache_read: r.get(3)?,
                cache_write: r.get(4)?,
            })
        })?
        .collect::<Result<_, _>>()?)
}

/// footer 只算 Node 调试面板会算的行：本会话、主循环、完成（`model_request_completed`）。`+` 让来源与状态不参与选
/// 索引：没有统计信息时 SQLite 会选 `model_usage_query_source_idx`，扫遍所有会话的主回合行；按会话找只有本会话的行。
/// 修复线 F112（查看子代理时 footer 查它的子会话，tui-panels.md §1.7）：来源加上 `subagent`。子代理的行只写在子会话
/// （Node 按会话的 taskType 写来源），主会话里没有，所以主会话的结果不变；修前子会话一行都不算，查看时没有命中率。
const COUNTED_REQUESTS: &str =
    "session_id = ?1 and +query_source in ('main_turn', 'subagent') and +status = 'completed'";

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

/// 量得到速度的行（子代理的平均速度，usage.md §4.1）：输出已知且大于 0，总时长大于首字时间。
fn measured() -> String {
    format!(
        "{output} = 1 and output_tokens > 0 and time_to_first_token_ms is not null
         and duration_ms > time_to_first_token_ms",
        output = known("outputTokens"),
    )
}

fn totals_sql() -> String {
    format!(
        "select count(*), coalesce(sum({INPUT_SIDE}), 0), coalesce(sum(cache_read_input_tokens), 0),
                coalesce(sum(1 - {input} * {read}), 0),
                coalesce(sum(case when {output} = 1 then output_tokens else 0 end), 0),
                coalesce(sum(case when {measured} then output_tokens else 0 end), 0),
                coalesce(sum(case when {measured} then duration_ms - time_to_first_token_ms else 0 end), 0)
           from model_usage where {COUNTED_REQUESTS}",
        input = known("inputTokens"),
        read = known("cacheReadTokens"),
        output = known("outputTokens"),
        measured = measured(),
    )
}

fn last_sql() -> String {
    format!(
        "select case when {output} = 1 then output_tokens end, duration_ms, time_to_first_token_ms
           from model_usage where {COUNTED_REQUESTS}
          order by completed_at desc, started_at desc, rowid desc limit 1",
        output = known("outputTokens"),
    )
}

/// 本会话计入 footer 的请求（[`totals_sql`]）：个数、输入一侧的 token、缓存读、用量不全的行数；已知的输出之和，与
/// 量得到速度的行的输出与生成时长之和（子代理的 token 与平均速度）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RequestTotals {
    pub requests: i64,
    pub input: i64,
    pub cache_read: i64,
    pub unknown: i64,
    pub output: i64,
    pub measured_output: i64,
    pub generation_ms: i64,
}

pub fn request_totals(conn: &Connection, session: &str) -> Result<RequestTotals> {
    Ok(conn
        .prepare_cached(&totals_sql())?
        .query_row([session], |r| {
            Ok(RequestTotals {
                requests: r.get(0)?,
                input: r.get(1)?,
                cache_read: r.get(2)?,
                unknown: r.get(3)?,
                output: r.get(4)?,
                measured_output: r.get(5)?,
                generation_ms: r.get(6)?,
            })
        })?)
}

/// 最近完成的一次计入的请求（[`last_sql`]）：输出（用量里有时）、耗时、首字的时刻。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LastRequestRow {
    pub output_tokens: Option<i64>,
    pub duration_ms: Option<i64>,
    pub first_token_ms: Option<i64>,
}

pub fn last_request(conn: &Connection, session: &str) -> Result<Option<LastRequestRow>> {
    Ok(conn
        .prepare_cached(&last_sql())?
        .query_row([session], |r| {
            Ok(LastRequestRow {
                output_tokens: r.get(0)?,
                duration_ms: r.get(1)?,
                first_token_ms: r.get(2)?,
            })
        })
        .optional()?)
}

#[cfg(test)]
#[path = "usage_reports_tests.rs"]
mod tests;
