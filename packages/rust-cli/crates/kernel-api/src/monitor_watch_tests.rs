//! 一个 monitor 的切行、批与限流（tools-more.md §19.6）：时刻由测试给，不用等。

use crate::env::EnvPath;
use crate::ids::{OperationId, ShellTaskId, ToolUseId};
use crate::monitor::{MonitorArmed, MonitorSource};

use super::*;

fn watch(timeout_ms: u64, start: u64) -> Watch {
    Watch::new(
        MonitorArmed {
            machine: None,
            operation: OperationId::new("op_1"),
            task: ShellTaskId::new("b1"),
            tool_use: ToolUseId::new("toolu_1"),
            description: "errors".to_owned(),
            timeout_ms,
            source: MonitorSource::Command,
            output: EnvPath::new("/t/b1.output"),
        },
        start,
    )
}

fn ms(value: u64) -> u64 {
    value
}

fn event(text: &str) -> Emit {
    Emit::Event(text.to_owned())
}

#[test]
fn lines_are_trimmed_batched_and_blank_lines_dropped() {
    let start = 1_000_000;
    let mut watch = watch(300_000, start);
    watch.on_data(b"  one  \n\n   \ntw", start);
    watch.on_data(b"o\nthr", start + ms(50));
    assert_eq!(watch.deadline(), Some(start + ms(200)));
    assert!(watch.flush_due(start + ms(199)).emits.is_empty());
    let step = watch.flush_due(start + ms(200));
    assert_eq!(step.emits, vec![Emit::Event("one\ntwo".to_owned())]);
    assert_eq!(watch.events, 1);
    // 没有换行的半行留到结束时取出。
    assert_eq!(watch.finish(), Some("thr".to_owned()));
    assert!(watch.closed);
    assert_eq!(watch.finish(), None);
}

#[test]
fn long_lines_and_batches_are_truncated() {
    let start = 1_000_000;
    let mut watch = watch(300_000, start);
    let long = "x".repeat(600);
    watch.on_data(format!("{long}\n").as_bytes(), start);
    let step = watch.flush_due(start + BATCH_DELAY);
    assert_eq!(
        step.emits,
        vec![Emit::Event(format!("{}...(truncated)", "x".repeat(500)))]
    );
    let many: String = (0..10).map(|_| format!("{}\n", "y".repeat(400))).collect();
    watch.on_data(many.as_bytes(), start + ms(300));
    let step = watch.flush_due(start + ms(300) + BATCH_DELAY);
    let Emit::Event(batch) = &step.emits[0] else {
        panic!("not an event");
    };
    assert!(batch.ends_with("\n...(truncated)"));
    assert_eq!(utf16_len(batch), 3_000 + "\n...(truncated)".len());
}

#[test]
fn the_bucket_suppresses_then_reports_and_refills() {
    let start = 1_000_000;
    let mut watch = watch(300_000, start);
    let mut now = start;
    let mut delivered = 0;
    for index in 0..12 {
        watch.on_data(format!("line {index}\n").as_bytes(), now);
        now += BATCH_DELAY;
        delivered += watch
            .flush_due(now)
            .emits
            .iter()
            .filter(|emit| matches!(emit, Emit::Event(_)))
            .count();
    }
    // 12 批里前 10 批有令牌，后两批压下；2.4 s 里补了 1 个令牌。
    assert_eq!(delivered, 11);
    watch.on_data(b"after\n", now);
    now += ms(2_000);
    let step = watch.flush_due(now);
    assert_eq!(
        step.emits,
        vec![Emit::Notice(Housekeeping::Suppressed(1)), event("after")]
    );
}

#[test]
fn suppressing_for_thirty_seconds_stops_the_monitor() {
    let start = 1_000_000;
    let mut watch = watch(300_000, start);
    let mut now = start;
    // 用光令牌。
    for _ in 0..10 {
        watch.on_data(b"x\n", now);
        now += BATCH_DELAY;
        watch.flush_due(now);
    }
    let first_suppressed = now + BATCH_DELAY;
    let mut stopped = None;
    // 每 200 ms 一批，补令牌（每 2 s 一个）赶不上；一直压下到 30 s 以后。
    for _ in 0..200 {
        watch.on_data(b"x\n", now);
        now += BATCH_DELAY;
        let step = watch.flush_due(now);
        if step.stop {
            stopped = Some((step, now));
            break;
        }
    }
    let (step, at) = stopped.expect("the monitor stops");
    assert!(watch.closed);
    let Some(Emit::Notice(Housekeeping::TooMuchOutput {
        suppressed,
        seconds,
    })) = step.emits.last()
    else {
        panic!("no notice: {step:?}");
    };
    assert!(*suppressed > 0);
    assert_eq!(*seconds, (at - first_suppressed + 500) / 1000);
    assert!(watch.stopped);
    // 关闭之后不再送。
    watch.on_data(b"late\n", now);
    assert!(watch.flush_due(now + BATCH_DELAY).emits.is_empty());
}

#[test]
fn expiry_flushes_pending_lines_then_reports_the_count() {
    let start = 1_000_000;
    let mut watch = watch(3_000, start);
    watch.on_data(b"ready\npartial", start + ms(1_000));
    assert_eq!(watch.deadline(), Some(start + ms(1_200)));
    let step = watch.expire(start + ms(1_100));
    assert!(step.stop && watch.expired && !watch.closed);
    assert_eq!(
        step.emits,
        vec![
            event("ready"),
            Emit::Notice(Housekeeping::Expired {
                timeout_ms: 3_000,
                events: 1
            })
        ]
    );
    assert!(watch.expire(start + ms(3_000)).emits.is_empty());
    // 停掉之后流结束：半行照样取出（作为普通事件送）。
    assert_eq!(watch.finish(), Some("partial".to_owned()));
    let quiet = &mut self::watch(60_000, start);
    assert_eq!(
        quiet.expire(start + ms(60_000)).emits,
        vec![Emit::Notice(Housekeeping::Expired {
            timeout_ms: 60_000,
            events: 0
        })]
    );
}

#[test]
fn the_buffer_keeps_the_last_mebibyte() {
    let start = 1_000_000;
    let mut watch = watch(300_000, start);
    let mut big = vec![b'a'; 2 * BUFFER_LIMIT];
    big.extend_from_slice(b"\nend\n");
    // 前 1 MiB 被丢掉，剩下的那一行（全是 a）截到 500。
    watch.on_data(&big, start);
    let step = watch.flush_due(start + BATCH_DELAY);
    assert_eq!(
        step.emits,
        vec![Emit::Event(format!(
            "{}...(truncated)\nend",
            "a".repeat(500)
        ))]
    );
}

fn socket_watch(start: u64) -> Watch {
    let mut watch = watch(60_000, start);
    watch.armed.source = MonitorSource::Socket;
    watch
}

/// ws 的生命周期：关闭前送出待发的帧，再送关闭；升级被拒之后不送出错；帧太大之后不送关闭。
#[test]
fn socket_lifecycle_follows_the_order_and_silences() {
    let start = 1_000_000;
    let mut watch = socket_watch(start);
    watch.on_data(b"frame\n", start);
    let step = watch.on_lifecycle(b"{\"type\":\"closed\",\"code\":1000}\n", start + 10);
    assert_eq!(
        step.emits,
        vec![
            event("frame"),
            Emit::Notice(Housekeeping::Socket(SocketEvent::Closed {
                code: 1000,
                reason: String::new()
            }))
        ]
    );
    assert!(watch.closed && watch.stopped);
    let mut rejected = socket_watch(start);
    let lines = b"{\"type\":\"rejected\",\"status\":403}\n{\"type\":\"error\",\"message\":\"x\"}\n";
    let step = rejected.on_lifecycle(lines, start);
    assert_eq!(
        step.emits,
        vec![Emit::Notice(Housekeeping::Socket(SocketEvent::Rejected {
            status: 403
        }))]
    );
    let mut dropped = socket_watch(start);
    let step = dropped.on_lifecycle(
        b"{\"type\":\"dropped\",\"bytes\":9}\n{\"type\":\"closed\",\"code\":1009}\n",
        start,
    );
    assert_eq!(
        step.emits,
        vec![Emit::Notice(Housekeeping::Socket(SocketEvent::Dropped {
            bytes: 9
        }))]
    );
}

/// 被停掉之后：剩下的行作为普通事件；用户停掉的 ws 另送 `Stopped`；到期之后的生命周期不再送。
#[test]
fn after_a_stop_the_rest_is_an_event() {
    let start = 1_000_000;
    let mut command = watch(60_000, start);
    assert_eq!(
        command
            .after_stop(Some("tail".to_owned()), false, start)
            .emits,
        vec![event("tail")]
    );
    let mut socket = socket_watch(start);
    assert_eq!(
        socket.after_stop(None, true, start).emits,
        vec![Emit::Notice(Housekeeping::Stopped)]
    );
    let mut expired = socket_watch(start);
    expired.expire(start + 60_000);
    assert!(
        expired
            .after_stop(None, true, start + 60_001)
            .emits
            .is_empty()
    );
    assert!(
        expired
            .on_lifecycle(b"{\"type\":\"closed\",\"code\":1000}\n", start + 60_002)
            .emits
            .is_empty()
    );
}
