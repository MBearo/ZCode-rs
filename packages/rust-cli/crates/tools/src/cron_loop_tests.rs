use std::time::Duration;

use rust_cli_kernel_api::cron::CronSignals;
use rust_cli_kernel_api::ids::SessionId;

use super::*;
use crate::cron_board::tests::TokioClock;

/// TokioClock 的起点 2026-10-17T10:07:30Z。
const START: u64 = 1_792_231_650_000;
const MINUTE: u64 = 60_000;

fn owner() -> SessionId {
    SessionId::new("sess_1")
}

async fn advance(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

fn wakeups(board: &CronBoard) -> Vec<crate::cron_board::Job> {
    board
        .session_jobs(&owner())
        .into_iter()
        .filter(|job| job.kind == JobKind::Loop)
        .collect()
}

/// 夹到 [60, 3600]、向上取整到整分钟、短时长挪进 cache 窗口。
#[test]
fn clamp_keeps_the_delay_in_range() {
    let now = START;
    // 1200 秒：10:27:30 → 10:28:00，createdAt 是 10:27:30。
    assert_eq!(
        clamp(1200.0, now),
        (
            1200,
            false,
            START + 20 * MINUTE + 30_000,
            START + 20 * MINUTE
        )
    );
    // 30 秒夹到 60：10:08:30 → 10:09:00。
    assert_eq!(clamp(30.0, now), (60, true, START + 90_000, START + MINUTE));
    // 290 秒：10:12:20 → 10:13:00，超出 285 秒的窗口，挪回 10:12:00；createdAt 是目标前 1 毫秒。
    let target = START + 4 * MINUTE + 30_000;
    assert_eq!(clamp(290.0, now), (290, false, target, target - 1));
    assert_eq!(clamp(59.5, now).0, 60);
    assert!(!clamp(59.5, now).1, "Math.round(59.5) is 60: not clamped");
    assert_eq!(clamp(4000.0, now).0, 3600);
    assert_eq!(clamp(f64::NAN, now).0, 60);
    assert_eq!(clamp(f64::INFINITY, now).0, 3600);
    assert!(clamp(f64::NEG_INFINITY, now).1);
}

/// 排一个 loop 唤醒（只留一个），到点空闲触发；之后模型没有再排 → keepalive 排 1200 秒（只一次）。
#[tokio::test(start_paused = true)]
async fn wakeups_fire_and_keepalive_once() {
    let board = CronBoard::new(TokioClock::new(), None);
    let first = board.schedule_wakeup(&owner(), "aaaa0001".to_owned(), 1200.0, "/loop check");
    assert_eq!(
        first,
        Scheduled::At {
            scheduled_for: START + 20 * MINUTE + 30_000,
            clamped: 1200,
            was_clamped: false
        }
    );
    board.schedule_wakeup(&owner(), "bbbb0002".to_owned(), 1200.0, "/loop check");
    let pending = wakeups(&board);
    assert_eq!(pending.len(), 1, "a new wakeup supersedes the old one");
    assert_eq!(pending[0].cron, "28 10 * * *");
    assert!(board.take_due(&owner()).await.is_empty());
    advance(21 * MINUTE).await;
    let fires = board.take_due(&owner()).await;
    assert_eq!(fires.len(), 1);
    assert_eq!(fires[0].prompt, "/loop check");
    // 这个 tick 结束了，模型没有再排：下次空闲时 keepalive 排 1200 秒。
    assert!(board.take_due(&owner()).await.is_empty());
    assert_eq!(wakeups(&board).len(), 1);
    advance(21 * MINUTE).await;
    assert_eq!(board.take_due(&owner()).await.len(), 1);
    // 预算用完：循环结束。
    assert!(board.take_due(&owner()).await.is_empty());
    assert!(wakeups(&board).is_empty());
}

/// 模型自己排唤醒时 keepalive 计数清零；`stop` 删掉唤醒并清掉在跑的 tick。
#[tokio::test(start_paused = true)]
async fn stop_cancels_wakeups_and_the_tick() {
    let board = CronBoard::new(TokioClock::new(), None);
    board.schedule_wakeup(&owner(), "aaaa0001".to_owned(), 60.0, "/loop check");
    assert_eq!(board.stop_loop(&owner()), 1);
    assert_eq!(board.stop_loop(&owner()), 0);
    board.schedule_wakeup(&owner(), "aaaa0001".to_owned(), 60.0, "/loop check");
    advance(2 * MINUTE).await;
    assert_eq!(board.take_due(&owner()).await.len(), 1);
    // tick 里模型停了：不 keepalive。
    assert_eq!(board.stop_loop(&owner()), 0);
    assert!(board.take_due(&owner()).await.is_empty());
    assert!(wakeups(&board).is_empty());
}

/// 一条链从开始满 7 天不再排；一分钟后链算断开，可以重新开始。
#[tokio::test(start_paused = true)]
async fn chains_end_after_seven_days() {
    let board = CronBoard::new(TokioClock::new(), None);
    let schedule = || board.schedule_wakeup(&owner(), "aaaa0001".to_owned(), 3600.0, "/loop x");
    assert!(matches!(schedule(), Scheduled::At { .. }));
    for _ in 0..(7 * 24 * 60 / 59) {
        advance(59 * MINUTE).await;
        assert!(matches!(schedule(), Scheduled::At { .. }));
    }
    advance(59 * MINUTE).await;
    assert_eq!(schedule(), Scheduled::Ended);
    assert_eq!(schedule(), Scheduled::Ended);
    advance(2 * MINUTE).await;
    assert!(matches!(schedule(), Scheduled::At { .. }));
}

/// 用户中断（tools-more.md §18.1）：取消全部 loop 唤醒、清掉在跑的 tick 与 keepalive 计数；
/// CronCreate 的循环任务不受影响。
#[tokio::test(start_paused = true)]
async fn a_user_interrupt_cancels_every_loop_wakeup() {
    let board = CronBoard::new(TokioClock::new(), None);
    board.add(
        &owner(),
        "cccc0001".to_owned(),
        "*/5 * * * *",
        "cron job",
        true,
    );
    board.schedule_wakeup(&owner(), "aaaa0001".to_owned(), 60.0, "/loop check");
    // 到点触发，tick 在跑；模型在这个 tick 里又排了一个。
    advance(2 * MINUTE).await;
    let fired = board.take_due(&owner()).await;
    assert!(fired.iter().any(|fire| fire.prompt == "/loop check"));
    board.schedule_wakeup(&owner(), "aaaa0002".to_owned(), 600.0, "/loop check");
    assert_eq!(wakeups(&board).len(), 1);
    assert_eq!(board.cancel_loops(&owner()).await, 1);
    assert!(wakeups(&board).is_empty());
    // 别的会话与 CronCreate 的任务不动；再中断一次没有可取消的。
    assert_eq!(board.session_count(&owner()), 1);
    assert_eq!(board.cancel_loops(&owner()).await, 0);
    // 在跑的 tick 已清掉：下一次空闲取任务时不会 keepalive。
    advance(30 * MINUTE).await;
    let fires = board.take_due(&owner()).await;
    assert!(
        fires.iter().all(|fire| fire.prompt != "/loop check"),
        "{fires:?}"
    );
    assert!(wakeups(&board).is_empty());
}
