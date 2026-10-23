use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use rust_cli_kernel_api::cron::CronSignals;
use rust_cli_kernel_api::cron_store::{CronCreator, CronTasksChange, StoredCronTask};
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::testing::MemoryCronStore;

use super::*;
use crate::cron_board::tests::TokioClock;

/// TokioClock 的起点 2026-10-17T10:07:30Z。
const START: u64 = 1_792_231_650_000;
const HOUR: u64 = 3_600_000;

fn me() -> SessionId {
    SessionId::new("sess_1")
}

fn task(id: &str, cron: &str, created_by: Option<&str>, creator: CronCreator) -> StoredCronTask {
    StoredCronTask {
        id: id.to_owned(),
        cron: cron.to_owned(),
        prompt: format!("prompt {id}"),
        created_at: START - HOUR,
        last_fired_at: None,
        recurring: true,
        permanent: false,
        created_by: created_by.map(SessionId::new),
        creator,
        in_project: true,
    }
}

fn board(store: &Arc<MemoryCronStore>) -> CronBoard {
    CronBoard::new(TokioClock::new(), Some(store.clone()))
}

async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// 等唤醒流产出一项（暂停的时间会自动推进），最多等 `ms`。
async fn woke(stream: &mut futures_util::stream::BoxStream<'static, ()>, ms: u64) -> bool {
    tokio::time::timeout(Duration::from_millis(ms), stream.next())
        .await
        .is_ok()
}

#[test]
fn belongs_picks_the_owning_session() {
    let session = me();
    let mine = task("a", "* * * * *", Some("sess_1"), CronCreator::Running);
    let legacy = task("b", "* * * * *", None, CronCreator::Gone);
    let same_process = task("c", "* * * * *", Some("sess_old"), CronCreator::ThisProcess);
    let orphan = task("d", "* * * * *", Some("sess_x"), CronCreator::Gone);
    let alive = task("e", "* * * * *", Some("sess_x"), CronCreator::Running);
    let elsewhere = StoredCronTask {
        in_project: false,
        ..orphan.clone()
    };
    for leader in [false, true] {
        assert!(belongs(&mine, &session, leader));
        assert!(belongs(&same_process, &session, leader));
        assert!(!belongs(&alive, &session, leader));
        assert!(!belongs(&elsewhere, &session, leader));
        assert_eq!(belongs(&legacy, &session, leader), leader);
        assert_eq!(belongs(&orphan, &session, leader), leader);
    }
}

/// 持久任务：写文件、首次唤醒时抢锁，到点空闲触发并回写 `lastFiredAt`；一次性任务触发后删除。
#[tokio::test(start_paused = true)]
async fn durable_tasks_fire_and_write_back() {
    let store = Arc::new(MemoryCronStore::default());
    let board = board(&store);
    let mut wakes = board.wakeups(&me());
    let task = |id: &str, cron: &str, recurring| NewCronTask {
        id: id.to_owned(),
        cron: cron.to_owned(),
        prompt: format!("prompt {id}"),
        created_at: board.now_ms(),
        recurring,
        created_by: me(),
    };
    assert!(
        board
            .add_durable(task("1a2b3c4d", "*/5 * * * *", true))
            .await
            .unwrap()
    );
    // 10:12 不落在 :00 / :30，没有抖动。
    assert!(
        board
            .add_durable(task("5e6f7a8b", "12 10 * * *", false))
            .await
            .unwrap()
    );
    assert!(!woke(&mut wakes, 60_000).await, "nothing is due yet");
    assert_eq!(
        store.holder(),
        Some(me()),
        "the first session takes the lock"
    );
    assert!(woke(&mut wakes, 10 * 60_000).await);
    // 循环任务 10:10:15 到期（抖动 15 秒）；会话忙，等到 10:12 一次性任务也到期再取。
    advance(2 * 60_000).await;
    let fires = board.take_due(&me()).await;
    let jobs: Vec<&str> = fires.iter().map(|fire| fire.job.as_str()).collect();
    assert_eq!(jobs, ["1a2b3c4d", "5e6f7a8b"]);
    let left = store.snapshot();
    assert_eq!(left.len(), 1, "the one-shot task is removed");
    assert_eq!(left[0].last_fired_at, Some(board.now_ms()));
    assert!(store.changes_made().iter().any(|change| matches!(
        change,
        CronTasksChange::MarkFired { ids, .. } if ids == &["1a2b3c4d".to_owned()]
    )));
}

/// 启动时错过的一次性任务：删除、排一条提示（立即唤醒）；错过的循环任务不提示，首次空闲触发一次。
#[tokio::test(start_paused = true)]
async fn missed_one_shot_tasks_are_asked_about_first() {
    let mut one_shot = task("aaaa0001", "0 9 * * *", Some("sess_1"), CronCreator::Gone);
    one_shot.recurring = false;
    one_shot.created_at = START - 30 * HOUR;
    let mut recurring = task("bbbb0002", "0 9 * * *", Some("sess_1"), CronCreator::Gone);
    recurring.created_at = START - 30 * HOUR;
    let store = Arc::new(MemoryCronStore::with_tasks(vec![one_shot, recurring]));
    let board = board(&store);
    let mut wakes = board.wakeups(&me());
    assert!(woke(&mut wakes, 1_000).await);
    let fires = board.take_due(&me()).await;
    assert_eq!(fires[0].job, MISSED_JOB);
    assert!(fires[0].prompt.starts_with(
        "The following one-shot scheduled task was missed while ZCode was not running. It has already been removed from .zcode/scheduled_tasks.json."
    ));
    assert!(fires[0].prompt.ends_with(
        "[Every day at 9:00 AM, created 10/16/2026, 4:07:30 AM]\n```\nprompt aaaa0001\n```"
    ));
    // 循环任务：从 createdAt 起的 9:00 已过，首次检查就到期，触发一次后从现在排下一次。
    assert_eq!(fires[1].job, "bbbb0002");
    assert_eq!(fires.len(), 2);
    assert_eq!(store.snapshot().len(), 1);
    assert!(board.take_due(&me()).await.is_empty());
}

/// 别的活着的进程持锁：只触发自己建的任务；持有者死了，下一次探测（5 秒）拿到锁，开始认领无主与孤儿任务。
#[tokio::test(start_paused = true)]
async fn the_lock_decides_who_fires_ownerless_tasks() {
    let store = Arc::new(MemoryCronStore::with_tasks(vec![
        task(
            "aaaa0001",
            "*/5 * * * *",
            Some("sess_1"),
            CronCreator::Running,
        ),
        task("bbbb0002", "*/5 * * * *", None, CronCreator::Gone),
        task(
            "cccc0003",
            "*/5 * * * *",
            Some("sess_dead"),
            CronCreator::Gone,
        ),
        task(
            "dddd0004",
            "*/5 * * * *",
            Some("sess_live"),
            CronCreator::Running,
        ),
    ]));
    store.hold_elsewhere();
    let board = board(&store);
    let mut wakes = board.wakeups(&me());
    assert!(woke(&mut wakes, 1_000).await);
    let fires = board.take_due(&me()).await;
    let jobs: Vec<&str> = fires.iter().map(|fire| fire.job.as_str()).collect();
    assert_eq!(jobs, ["aaaa0001"], "not the leader: only our own task");
    store.holder_died();
    advance(6_000).await;
    let _ = woke(&mut wakes, 1).await;
    assert_eq!(store.holder(), Some(me()));
    let fires = board.take_due(&me()).await;
    let jobs: Vec<&str> = fires.iter().map(|fire| fire.job.as_str()).collect();
    assert_eq!(jobs, ["bbbb0002", "cccc0003"]);
}

/// 别的会话写进文件的任务经监听回到投影；会话关闭后释放锁、不再触发持久任务。
#[tokio::test(start_paused = true)]
async fn external_writes_reach_the_board_and_close_stops_it() {
    let store = Arc::new(MemoryCronStore::default());
    let board = board(&store);
    board.enable();
    let mut wakes = board.wakeups(&me());
    assert!(!woke(&mut wakes, 1_000).await);
    let mut external = task("eeee0005", "*/5 * * * *", None, CronCreator::Gone);
    external.created_at = board.now_ms();
    store.push_external(external);
    assert!(woke(&mut wakes, 10 * 60_000).await);
    assert_eq!(board.take_due(&me()).await[0].job, "eeee0005");
    board.session_closed(&me()).await;
    assert_eq!(store.holder(), None);
    advance(10 * 60_000).await;
    assert!(board.take_due(&me()).await.is_empty());
}

/// 复现（R1-1）：会话关闭后可以再打开（新的 actor 订阅新的唤醒流）。以前关闭把调度器标成永久关闭，再打开的
/// 会话里持久任务再也不触发；现在关闭清掉这个会话的调度状态，新的唤醒流重新抢锁、重新调度。
#[tokio::test(start_paused = true)]
async fn a_reopened_session_schedules_durable_tasks_again() {
    let store = Arc::new(MemoryCronStore::default());
    let board = board(&store);
    board.enable();
    let wakes = board.wakeups(&me());
    drop(wakes);
    board.session_closed(&me()).await;
    assert_eq!(store.holder(), None);
    let mut again = board.wakeups(&me());
    let mut external = task("eeee0005", "*/5 * * * *", None, CronCreator::Gone);
    external.created_at = board.now_ms();
    store.push_external(external);
    assert!(woke(&mut again, 10 * 60_000).await);
    assert_eq!(store.holder(), Some(me()));
    assert_eq!(board.take_due(&me()).await[0].job, "eeee0005");
}

/// 满 7 天的循环任务最后触发一次后删除；`permanent` 不到期。
#[tokio::test(start_paused = true)]
async fn aged_tasks_expire_unless_permanent() {
    let mut aged = task(
        "aaaa0001",
        "*/5 * * * *",
        Some("sess_1"),
        CronCreator::Running,
    );
    aged.created_at = START - 8 * 24 * HOUR;
    aged.last_fired_at = Some(START - 60_000);
    let permanent = StoredCronTask {
        id: "bbbb0002".to_owned(),
        permanent: true,
        ..aged.clone()
    };
    let store = Arc::new(MemoryCronStore::with_tasks(vec![aged, permanent]));
    let board = board(&store);
    let mut wakes = board.wakeups(&me());
    assert!(!woke(&mut wakes, 1_000).await);
    // 两个任务的抖动不同（10:11:40、10:11:50），都到期后再取。
    advance(5 * 60_000).await;
    assert_eq!(board.take_due(&me()).await.len(), 2);
    let left = store.snapshot();
    assert_eq!(left.len(), 1);
    assert_eq!(left[0].id, "bbbb0002");
}
