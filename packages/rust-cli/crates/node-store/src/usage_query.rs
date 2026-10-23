//! `/usage` 的两个只读查询（docs/specs/rust-cli/usage.md §4、§5.2）：都是聚合 SQL，不把消息或用量行整表读进
//! 内存。会话的开始与时长、按天的消息数只扫 `message_session_time_created_id_idx`。
use rusqlite::Connection;
use rust_cli_kernel_api::clock::civil_date;
use rust_cli_kernel_api::metering::{
    ActivityDay, ActivityQuery, ActivityRange, ActivityStats, ModelTokens, SessionMetering,
};
use rust_cli_kernel_api::store::StoreError;

use crate::{OrStorage, storage};

/// 一天的毫秒数：只定义在 base 的 `js::date::DAY_MS`（u64，F58：以前这里另写一份 i64）。SQL 文本与 i64 的日界
/// 算术用这个局部的 i64 值，文本逐字不变。
const DAY_MS: i64 = rust_cli_base::js::date::DAY_MS as i64;

/// 会话及其子代理子会话（`parent_id` 递归）里成功请求的耗时之和（`Total duration (API)`）。
pub(crate) const METERING: &str = "with recursive tree(id) as (
       select ?1
       union
       select s.id from session s join tree t on s.parent_id = t.id
        where s.task_type = 'subagent_child'
     )
     select coalesce(sum(duration_ms), 0) from model_usage
      where session_id in (select id from tree) and status = 'completed'";

pub fn metering(conn: &Connection, session: &str) -> Result<SessionMetering, StoreError> {
    let total: i64 = conn
        .prepare_cached(METERING)
        .storage()?
        .query_row([session], |r| r.get(0))
        .storage()?;
    Ok(SessionMetering {
        api_duration_ms: u64::try_from(total).unwrap_or(0),
    })
}

/// 统计里的会话：未删除、不是子代理或工作流的子运行（子代理的 transcript 不统计）。
const COUNTED: &str = "s.time_deleted is null
       and s.task_type not in ('subagent_child', 'workflow_child', 'nested_workflow_child')";

/// 范围内开始的会话：个数、最长时长、最早与最晚的开始。开始与结束是这个会话消息的最早与最晚
/// 创建时间（每个会话两次索引上的 min / max）。
pub(crate) fn sessions_sql() -> String {
    format!(
        "with started as (
           select (select min(m.time_created) from message m where m.session_id = s.id) as first,
                  (select max(m.time_created) from message m where m.session_id = s.id) as last
             from session s where {COUNTED}
         )
         select count(*), max(last - first), min(first), max(first)
           from started where first is not null and first >= ?1"
    )
}

/// 按本地日的消息数（`dailyActivity.messageCount`）。`cross join` 固定会话在外层：每个会话一段覆盖索引。
pub(crate) fn days_sql() -> String {
    format!(
        "select cast((m.time_created + ?1) / {DAY_MS} as integer) as day, count(*)
           from session s cross join message m on m.session_id = s.id
          where {COUNTED} and m.time_created >= ?2
          group by day order by day"
    )
}

/// 按模型的 token（`modelUsage`）：transcript 里的请求（主线程、子代理、工作流子运行）。`input_tokens` 是
/// AI SDK 的总输入；不含缓存的输入按 Node `inputSideTokensFromStoredUsage` 的判别得出（旧行的输入不含缓存）。
pub(crate) const MODELS: &str = "select model_id,
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

/// `YYYY-MM-DD` → 自 1970-01-01 的天数（Howard Hinnant 的 days_from_civil）。
pub(crate) fn day_index(date: &str) -> Option<i64> {
    let mut parts = date.splitn(3, '-').map(str::parse::<i64>);
    let (year, month, day) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted = (month + 9) % 12;
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub fn activity(conn: &Connection, query: &ActivityQuery) -> Result<ActivityStats, StoreError> {
    let offset = i64::from(query.utc_offset_minutes) * 60_000;
    let today =
        day_index(&query.today).ok_or_else(|| storage(format!("not a date: {}", query.today)))?;
    // 7 天与 30 天从 today − (N − 1) 的本地日开始；本地日 d 从 UTC 毫秒 d·DAY − 偏移开始。
    let from = match query.range {
        ActivityRange::All => i64::MIN,
        range => {
            let days = i64::try_from(range.days().unwrap_or(0)).unwrap_or(0);
            (today - (days - 1)) * DAY_MS - offset
        }
    };
    let (sessions, longest, first, last) = conn
        .prepare_cached(&sessions_sql())
        .storage()?
        .query_row([from], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, Option<i64>>(1)?,
                r.get::<_, Option<i64>>(2)?,
                r.get::<_, Option<i64>>(3)?,
            ))
        })
        .storage()?;
    let days = conn
        .prepare_cached(&days_sql())
        .storage()?
        .query_map([offset, from], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        })
        .storage()?
        .map(|row| {
            row.map(|(day, messages)| ActivityDay {
                date: civil_date(to_u64(day) * rust_cli_base::js::date::DAY_MS),
                messages: to_u64(messages),
            })
        })
        .collect::<Result<Vec<_>, _>>()
        .storage()?;
    let mut models = conn
        .prepare_cached(MODELS)
        .storage()?
        .query_map([from], |r| {
            Ok(ModelTokens {
                model: r.get(0)?,
                input: to_u64(r.get(1)?),
                output: to_u64(r.get(2)?),
                cache_read: to_u64(r.get(3)?),
                cache_write: to_u64(r.get(4)?),
            })
        })
        .storage()?
        .collect::<Result<Vec<_>, _>>()
        .storage()?;
    // 按总数降序；相同时按模型名，结果稳定。
    models.sort_by(|a, b| {
        b.total()
            .cmp(&a.total())
            .then_with(|| a.model.cmp(&b.model))
    });
    models.retain(|model| model.total() > 0);
    Ok(ActivityStats {
        range: Some(query.range),
        today: query.today.clone(),
        days,
        models,
        sessions: to_u64(sessions),
        longest_session_ms: longest.map(to_u64),
        first_session_ms: first.map(to_u64),
        last_session_ms: last.map(to_u64),
    })
}

#[cfg(test)]
#[path = "usage_query_tests.rs"]
mod tests;
