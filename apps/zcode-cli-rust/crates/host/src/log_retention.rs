//! Log retention (Node `adapters/src/logging/retention.ts`): once, 60 s after
//! start, delete this runtime's daily files dated before `today - 7 + 1`.
use chrono::{Days, NaiveDate};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const RETENTION_DAYS: u64 = 7;
pub const STARTUP_DELAY: Duration = Duration::from_secs(60);
const PREFIX: &str = "zcode-rust-";
const SUFFIX: &str = ".jsonl";

#[derive(Debug, Default, PartialEq)]
pub struct Cleanup {
    pub cutoff: String,
    pub scanned: usize,
    pub deleted: Vec<String>,
    pub failed: Vec<String>,
}

/// `zcode-rust-YYYY-MM-DD.jsonl` with a valid calendar date.
fn file_date(name: &str) -> Option<NaiveDate> {
    let date = name.strip_prefix(PREFIX)?.strip_suffix(SUFFIX)?;
    let shaped = date.len() == 10
        && date.bytes().enumerate().all(|(i, b)| match i {
            4 | 7 => b == b'-',
            _ => b.is_ascii_digit(),
        });
    shaped.then(|| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())?
}

/// Deletes the files of `dir` dated before `today - retention_days + 1`.
pub async fn cleanup(dir: &Path, today: NaiveDate, retention_days: u64) -> Cleanup {
    let cutoff = today
        .checked_sub_days(Days::new(retention_days.max(1) - 1))
        .unwrap_or(today);
    let mut result = Cleanup {
        cutoff: cutoff.format("%Y-%m-%d").to_string(),
        ..Default::default()
    };
    let mut entries = match tokio::fs::read_dir(dir).await {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return result,
        Err(error) => {
            tracing::warn!(
                target: "zcode::logging",
                event = "log.retention.cleanup.failed",
                cutoff_date = result.cutoff.as_str(),
                error_kind = ?error.kind(),
                "Log retention cleanup failed"
            );
            return result;
        }
    };
    while let Ok(Some(entry)) = entries.next_entry().await {
        let name = entry.file_name().to_string_lossy().into_owned();
        let is_file = entry.file_type().await.is_ok_and(|t| t.is_file());
        let Some(date) = file_date(&name).filter(|_| is_file) else {
            continue;
        };
        result.scanned += 1;
        if date >= cutoff {
            continue;
        }
        match tokio::fs::remove_file(entry.path()).await {
            Ok(()) => result.deleted.push(name),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                tracing::warn!(
                    target: "zcode::logging",
                    event = "log.retention.delete.failed",
                    file_name = name.as_str(),
                    error_kind = ?error.kind(),
                    "Log retention file delete failed"
                );
                result.failed.push(name);
            }
        }
    }
    tracing::debug!(
        target: "zcode::logging",
        event = "log.retention.cleanup.completed",
        cutoff_date = result.cutoff.as_str(),
        scanned_files = result.scanned,
        deleted_file_count = result.deleted.len(),
        failed_file_count = result.failed.len(),
        "Log retention cleanup completed"
    );
    result
}

/// Runs [`cleanup`] once after [`STARTUP_DELAY`] in the background.
pub fn schedule(dir: PathBuf) {
    tracing::info!(
        target: "zcode::logging",
        event = "log.retention.cleanup.scheduled",
        delay_ms = STARTUP_DELAY.as_millis() as u64,
        retention_days = RETENTION_DAYS,
        "Log retention cleanup scheduled"
    );
    tokio::spawn(async move {
        tokio::time::sleep(STARTUP_DELAY).await;
        cleanup(&dir, chrono::Local::now().date_naive(), RETENTION_DAYS).await;
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn deletes_only_own_files_before_the_cutoff() {
        let dir = std::env::temp_dir().join(format!("zcode-log-retention-{}", crate::id()));
        tokio::fs::create_dir_all(dir.join("zcode-rust-2026-09-10.jsonl.d"))
            .await
            .unwrap();
        for name in [
            "zcode-rust-2026-09-17.jsonl",
            "zcode-rust-2026-09-18.jsonl",
            "zcode-rust-2026-09-24.jsonl",
            "zcode-rust-2026-02-30.jsonl",
            "zcode-rust-2026-9-01.jsonl",
            "zcode-2026-09-01.jsonl",
            "other.txt",
        ] {
            tokio::fs::write(dir.join(name), "").await.unwrap();
        }
        let today = NaiveDate::from_ymd_opt(2026, 9, 24).unwrap();
        let result = cleanup(&dir, today, RETENTION_DAYS).await;
        assert_eq!(result.cutoff, "2026-09-18");
        assert_eq!(result.scanned, 3);
        assert_eq!(result.deleted, ["zcode-rust-2026-09-17.jsonl"]);
        assert!(result.failed.is_empty());
        let mut left = vec![];
        let mut entries = tokio::fs::read_dir(&dir).await.unwrap();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            left.push(entry.file_name().to_string_lossy().into_owned());
        }
        left.sort();
        assert_eq!(left.len(), 7);
        assert!(!left.contains(&"zcode-rust-2026-09-17.jsonl".to_owned()));
        tokio::fs::remove_dir_all(&dir).await.unwrap();
        assert_eq!(cleanup(&dir, today, 7).await.scanned, 0);
    }
}
