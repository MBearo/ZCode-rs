use rust_cli_kernel_api::cron_store::{CronCreator, StoredCronTask};

use super::*;

const UTC: fn(u64) -> i32 = |_| 0;

fn task(id: &str, prompt: &str, created_at: u64) -> StoredCronTask {
    StoredCronTask {
        id: id.to_owned(),
        cron: "30 14 * * *".to_owned(),
        prompt: prompt.to_owned(),
        created_at,
        last_fired_at: None,
        recurring: false,
        permanent: false,
        created_by: None,
        creator: CronCreator::Gone,
        in_project: true,
    }
}

#[test]
fn locale_and_clock_times() {
    // 2026-10-17T10:07:30Z。
    let at = 1_792_231_650_000;
    assert_eq!(locale_string(at, &UTC), "10/17/2026, 10:07:30 AM");
    assert_eq!(clock_time(at, &UTC), "10:07:30");
    assert_eq!(
        locale_string(at + 4 * 3_600_000, &UTC),
        "10/17/2026, 2:07:30 PM"
    );
    assert_eq!(
        locale_string(at - 10 * 3_600_000, &UTC),
        "10/17/2026, 12:07:30 AM"
    );
    assert_eq!(clock_time(at, &|_| 480), "18:07:30");
}

/// 错过任务的提示：单数与复数、围栏比正文里最长的反引号串多一个。
#[test]
fn missed_notice_is_verbatim() {
    let one = missed(&[task("a", "remind me", 1_792_231_650_000)], &UTC);
    assert_eq!(
        one,
        "The following one-shot scheduled task was missed while ZCode was not running. It has already been removed from .zcode/scheduled_tasks.json.\n\nDo NOT execute this prompt yet. First use the AskUserQuestion tool to ask whether to run it now. Only execute if the user confirms.\n\n[Every day at 2:30 PM, created 10/17/2026, 10:07:30 AM]\n```\nremind me\n```"
    );
    let two = missed(
        &[
            task("a", "run ````x````", 1_792_231_650_000),
            task("b", "second", 1_792_231_650_000),
        ],
        &UTC,
    );
    assert!(two.starts_with("The following one-shot scheduled tasks were missed while ZCode was not running. They have already been removed"));
    assert!(two.contains("Do NOT execute these prompts yet. First use the AskUserQuestion tool to ask whether to run each one now."));
    assert!(two.contains("\n`````\nrun ````x````\n`````\n\n[Every day at 2:30 PM"));
}

#[test]
fn wakeup_results_are_verbatim() {
    assert_eq!(
        wakeup_scheduled("10:28:00", 1230, None),
        "Next wakeup scheduled for 10:28:00 (in 1230s). If you owe the user a status update this tick, write it now as ordinary response text; then end the turn \u{2014} the harness re-invokes you when the wakeup fires or a task-notification arrives."
    );
    assert!(
        wakeup_scheduled("10:09:00", 90, Some(60))
            .contains("(in 90s) (clamped to 60s from your requested value).")
    );
    assert_eq!(
        loop_stopped(0),
        "Loop stopped \u{2014} any dynamic loop in this session is ended; there was no pending wakeup to cancel. If you are running a fixed-interval /loop (a recurring cron), it is NOT stopped by this call \u{2014} cancel it with CronDelete. If you armed a Monitor for this loop, TaskStop it now. Then write the loop's outcome for the user as ordinary visible response text \u{2014} and end the turn."
    );
    assert_eq!(
        loop_stopped(2),
        "Loop stopped \u{2014} cancelled 2 pending wakeup(s); no further dynamic-loop wakeups scheduled. If you armed a Monitor for this loop, TaskStop it now. Then write the loop's outcome for the user as ordinary visible response text \u{2014} and end the turn."
    );
    assert!(
        wakeup_ended().starts_with("Wakeup not scheduled. The loop reached its maximum duration")
    );
}
