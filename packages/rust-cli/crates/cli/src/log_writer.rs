//! 日志的唯一写者（docs/specs/rust-cli/logging.md §1、§6）：按天选文件、每天的上限、7 天保留。
//! 文件 IO 经 `rust_cli_fs`；写失败只吞掉（日志不能打断执行路径，Node 相同）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rust_cli_kernel_api::clock::{Clock, civil_date};
use serde_json::{Map, Value};
use tokio::sync::mpsc;

use crate::log::{Item, notice};

/// 日志文件名 `zcode-<日期>.jsonl` 的前后缀（写与按日期清理共用，F55：以前各写一份）。
const FILE_PREFIX: &str = "zcode-";
const FILE_SUFFIX: &str = ".jsonl";
/// 保留天数（Node `LOG_RETENTION_DAYS`）。
const RETENTION_DAYS: i64 = 7;
/// 一次最多取这么多项，合成一次追加。
const BATCH: usize = 256;

/// 正在写的那一天。
struct Day {
    date: String,
    path: PathBuf,
    size: u64,
    capped: bool,
}

pub(crate) struct DailyFiles {
    dir: PathBuf,
    cap: u64,
    day: Option<Day>,
}

impl DailyFiles {
    pub(crate) fn new(dir: PathBuf, cap: u64) -> Self {
        Self {
            dir,
            cap,
            day: None,
        }
    }

    /// 追加一批行（每行以换行结尾）。日期变了先换文件并清理旧文件；到上限时写一行提示，当天其余的丢弃。
    pub(crate) async fn append(&mut self, today: &str, now_ms: u64, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let day = match &mut self.day {
            Some(day) if day.date == today => day,
            _ => self.day.insert(open(&self.dir, today, self.cap).await),
        };
        if day.capped {
            return;
        }
        let length = bytes.len() as u64;
        if day.size + length <= self.cap {
            if rust_cli_fs::append(&day.path, bytes).await.is_ok() {
                day.size += length;
            }
            return;
        }
        day.capped = true;
        let mut context = Map::new();
        context.insert("limitBytes".to_owned(), Value::from(self.cap));
        let mut line = notice(
            now_ms,
            "log.capped",
            "the daily log reached its size limit; later lines today are dropped",
            context,
        );
        line.push('\n');
        let _ = rust_cli_fs::append(&day.path, line.as_bytes()).await;
    }
}

/// 打开某一天：读已有文件的大小（续写同一天的文件），并删掉超过保留期的文件。
async fn open(dir: &Path, date: &str, cap: u64) -> Day {
    let path = dir.join(format!("{FILE_PREFIX}{date}{FILE_SUFFIX}"));
    let size = rust_cli_fs::file_len(&path)
        .await
        .ok()
        .flatten()
        .unwrap_or(0);
    if let Some(cutoff) = days_before(date, RETENTION_DAYS) {
        prune(dir, &cutoff).await;
    }
    Day {
        date: date.to_owned(),
        path,
        size,
        capped: size >= cap,
    }
}

/// 删掉日期早于 `cutoff` 的 `zcode-YYYY-MM-DD.jsonl`；名字不符合的文件不动（Node `cleanupLogRetention`）。
async fn prune(dir: &Path, cutoff: &str) {
    let Ok(names) = rust_cli_fs::file_names(dir).await else {
        return;
    };
    for name in names {
        let Some(date) = name
            .strip_prefix(FILE_PREFIX)
            .and_then(|rest| rest.strip_suffix(FILE_SUFFIX))
        else {
            continue;
        };
        if parse_date(date).is_some() && date < cutoff {
            let _ = rust_cli_fs::remove_optional(&dir.join(&name)).await;
        }
    }
}

/// `YYYY-MM-DD` → 自 1970-01-01 起的天数（Howard Hinnant 的 days_from_civil）。
fn parse_date(date: &str) -> Option<i64> {
    let mut parts = date.split('-');
    let (year, month, day) = (parts.next()?, parts.next()?, parts.next()?);
    if parts.next().is_some() || year.len() != 4 || month.len() != 2 || day.len() != 2 {
        return None;
    }
    let (year, month, day): (i64, i64, i64) =
        (year.parse().ok()?, month.parse().ok()?, day.parse().ok()?);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let shifted = if month > 2 { month - 3 } else { month + 9 };
    let day_of_year = (153 * shifted + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    Some(era * 146_097 + day_of_era - 719_468)
}

/// `date` 往前 `days` 天的日期。
pub(crate) fn days_before(date: &str, days: i64) -> Option<String> {
    let target = parse_date(date)? - days;
    let ms = u64::try_from(target).ok()?.checked_mul(86_400_000)?;
    Some(civil_date(ms))
}

/// 写者任务：一次取一批，补上丢弃提示，按当天的文件追加，再回复这一批里的刷新请求。所有发送方都释放后结束。
pub(crate) async fn write_loop(
    mut receiver: mpsc::Receiver<Item>,
    mut files: DailyFiles,
    clock: Arc<dyn Clock>,
    dropped: Arc<AtomicU64>,
) {
    let mut batch = Vec::with_capacity(BATCH);
    while receiver.recv_many(&mut batch, BATCH).await > 0 {
        let now_ms = clock.now_ms();
        let mut bytes = Vec::new();
        let missed = dropped.swap(0, Ordering::Relaxed);
        if missed > 0 {
            let mut context = Map::new();
            context.insert("dropped".to_owned(), Value::from(missed));
            bytes.extend_from_slice(
                notice(now_ms, "log.dropped", "log lines were dropped", context).as_bytes(),
            );
            bytes.push(b'\n');
        }
        let mut flushes = Vec::new();
        for item in batch.drain(..) {
            match item {
                Item::Line(line) => {
                    bytes.extend_from_slice(line.as_bytes());
                    bytes.push(b'\n');
                }
                Item::Flush(reply) => flushes.push(reply),
            }
        }
        files.append(&clock.today(), now_ms, &bytes).await;
        for reply in flushes {
            let _ = reply.send(());
        }
    }
}

#[cfg(test)]
#[path = "log_writer_tests.rs"]
mod tests;
