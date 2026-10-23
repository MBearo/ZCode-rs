use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::clock::Clock;
use rust_cli_kernel_api::cron::CronSignals;
use rust_cli_kernel_api::ids::SessionId;

use super::*;

/// 跟着 tokio 的（暂停的）时间走的时钟：测试里睡眠会自动推进时间。
pub(crate) struct TokioClock {
    base: u64,
    start: tokio::time::Instant,
}

impl TokioClock {
    /// 从 2026-10-17T10:07:30Z 开始。
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            base: 1_792_231_650_000,
            start: tokio::time::Instant::now(),
        })
    }
}

impl Clock for TokioClock {
    fn now_ms(&self) -> u64 {
        self.base + u64::try_from(self.start.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn time_zone(&self) -> String {
        "UTC".to_owned()
    }
}

fn owner() -> SessionId {
    SessionId::new("sess_1")
}

async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

#[tokio::test(start_paused = true)]
async fn one_shot_fires_once_when_due_and_is_removed() {
    let board = CronBoard::new(TokioClock::new(), None);
    // 10:12 不落在 :00 / :30，没有抖动。
    board.add(
        &owner(),
        "1a2b3c4d".to_owned(),
        "12 10 * * *",
        "ping",
        false,
    );
    assert!(board.take_due(&owner()).await.is_empty());
    advance(4 * 60_000 + 30_000).await;
    let fires = board.take_due(&owner()).await;
    assert_eq!(fires.len(), 1);
    assert_eq!(
        (fires[0].job.as_str(), fires[0].prompt.as_str()),
        ("1a2b3c4d", "ping")
    );
    assert_eq!(board.session_count(&owner()), 0);
    assert!(board.take_due(&owner()).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn recurring_job_reschedules_and_expires_after_seven_days() {
    let board = CronBoard::new(TokioClock::new(), None);
    board.add(&owner(), "00000000".to_owned(), "* * * * *", "tick", true);
    advance(60_000).await;
    assert_eq!(board.take_due(&owner()).await.len(), 1);
    assert_eq!(board.session_count(&owner()), 1);
    // 同一分钟里再取没有新的到期任务；下一分钟再触发一次。
    assert!(board.take_due(&owner()).await.is_empty());
    advance(60_000).await;
    assert_eq!(board.take_due(&owner()).await.len(), 1);
    // 满 7 天：最后触发一次后删除。
    advance(RECURRING_MAX_AGE_MS).await;
    assert_eq!(board.take_due(&owner()).await.len(), 1);
    assert_eq!(board.session_count(&owner()), 0);
}

#[tokio::test(start_paused = true)]
async fn wakeups_fire_once_per_due_time_and_follow_changes() {
    let board = CronBoard::new(TokioClock::new(), None);
    let mut wakeups = board.wakeups(&owner());
    // 还没有任务：等到任务表变化。
    let added = board.clone();
    let add = async move {
        advance(1_000).await;
        added.add(
            &owner(),
            "1a2b3c4d".to_owned(),
            "12 10 * * *",
            "ping",
            false,
        );
    };
    let (_, first) = tokio::join!(add, wakeups.next());
    assert_eq!(first, Some(()));
    let clock_after = board.now_ms();
    assert!(
        clock_after >= 1_792_231_920_000,
        "woke before 10:12: {clock_after}"
    );
    // 没有取走（会话忙）：同一个到期时间不再提醒，直到有新的任务时间。
    let quiet = tokio::time::timeout(Duration::from_secs(3_600), wakeups.next()).await;
    assert!(quiet.is_err());
    assert_eq!(board.take_due(&owner()).await.len(), 1);
}

#[tokio::test(start_paused = true)]
async fn jobs_are_kept_per_owner_session() {
    let board = CronBoard::new(TokioClock::new(), None);
    let other = SessionId::new("sess_2");
    board.add(&owner(), "a".to_owned(), "*/5 * * * *", "one", true);
    board.add(&other, "b".to_owned(), "*/5 * * * *", "two", true);
    assert!(board.has_session_job(&owner(), "a"));
    assert!(!board.has_session_job(&owner(), "b"));
    assert!(board.delete_session_job(&owner(), "a"));
    assert_eq!(board.session_count(&owner()), 0);
    assert_eq!(board.session_jobs(&other).len(), 1);
}
