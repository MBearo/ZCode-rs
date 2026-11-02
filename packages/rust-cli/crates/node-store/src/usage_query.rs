//! `/usage` 的两个只读查询（docs/specs/rust-cli/usage.md §4、§5.2）：SQL 在 `repo::usage_reports`（F138），这里是
//! 范围的换算与换成 kernel-api 的 metering 类型。会话的开始与时长、按天的消息数只扫 `message_session_time_created_id_idx`。
use rusqlite::Connection;
use rust_cli_base::{civil, units};
use rust_cli_kernel_api::clock::civil_date;
use rust_cli_kernel_api::metering::{
    ActivityDay, ActivityQuery, ActivityRange, ActivityStats, ModelTokens, SessionMetering,
};
use rust_cli_kernel_api::store::StoreError;

use crate::repo::usage_reports::{self as reports, DAY_MS};
use crate::{OrStorage, storage};

pub fn metering(conn: &Connection, session: &str) -> Result<SessionMetering, StoreError> {
    let total = reports::api_duration(conn, session).storage()?;
    Ok(SessionMetering {
        api_duration_ms: u64::try_from(total).unwrap_or(0),
    })
}

/// `YYYY-MM-DD` → 自 1970-01-01 的天数（宽松：三段都按整数读，不查位数与范围；公历换算只定义在 base 的 `civil`）。
/// 日数在换算里是线性的（`+ 日 − 1`），所以按 1 日换算再加上去，超出 1–31 的日数与以前一样往后（前）数。
pub(crate) fn day_index(date: &str) -> Option<i64> {
    // 年、月、日三段。
    const FIELDS: usize = 3;
    let mut parts = date.splitn(FIELDS, '-').map(str::parse::<i64>);
    let (year, month, day) = (
        parts.next()?.ok()?,
        parts.next()?.ok()?,
        parts.next()?.ok()?,
    );
    Some(civil::days_from_civil(year, u32::try_from(month).ok()?, 1) + day - 1)
}

fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub fn activity(conn: &Connection, query: &ActivityQuery) -> Result<ActivityStats, StoreError> {
    let offset = i64::from(query.utc_offset_minutes) * units::MS_PER_MINUTE as i64;
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
    let started = reports::started(conn, from).storage()?;
    let days = reports::days(conn, offset, from)
        .storage()?
        .into_iter()
        .map(|(day, messages)| ActivityDay {
            date: civil_date(to_u64(day) * rust_cli_base::js::date::DAY_MS),
            messages: to_u64(messages),
        })
        .collect();
    let mut models: Vec<ModelTokens> = reports::models(conn, from)
        .storage()?
        .into_iter()
        .map(|row| ModelTokens {
            model: row.model,
            input: to_u64(row.input),
            output: to_u64(row.output),
            cache_read: to_u64(row.cache_read),
            cache_write: to_u64(row.cache_write),
        })
        .collect();
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
        sessions: to_u64(started.count),
        longest_session_ms: started.longest.map(to_u64),
        first_session_ms: started.first.map(to_u64),
        last_session_ms: started.last.map(to_u64),
    })
}

#[cfg(test)]
#[path = "usage_query_tests.rs"]
mod tests;
