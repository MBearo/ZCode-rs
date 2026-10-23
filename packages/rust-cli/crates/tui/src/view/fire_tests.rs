//! 定时任务的触发行与 `/loop` 的折叠（tools-more.md §18.3）。
use rust_cli_kernel_api::cron::{CronFireKind, LoopFold};
use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOutcome};

use super::*;

fn view_with(events: Vec<KernelEvent>) -> Transcript {
    let selection = ModelSelection {
        provider: "fixture".into(),
        model: "scripted".into(),
        reasoning: None,
        connection: None,
    };
    let mut view = Transcript::new(
        selection,
        PermissionMode::Default,
        crate::view::test_workspace(),
    );
    for (index, event) in events.into_iter().enumerate() {
        view.apply(
            SequencedEvent {
                seq: index as u64 + 1,
                event,
            },
            0,
        );
    }
    view
}

fn fire(kind: CronFireKind, fold: Option<u32>) -> KernelEvent {
    KernelEvent::ScheduledFire {
        at: "Oct 18 3:04pm".into(),
        kind,
        fold: fold.map(|streak| LoopFold {
            streak,
            since: "Oct 18 2:44pm".into(),
        }),
    }
}

fn turn(n: &str) -> Vec<KernelEvent> {
    vec![
        KernelEvent::TurnStarted {
            turn: TurnId::new(n),
            command: None,
        },
        KernelEvent::TurnFinished {
            turn: TurnId::new(n),
            outcome: TurnOutcome::Success,
            usage: None,
        },
    ]
}

fn fires(view: &Transcript) -> Vec<Fire> {
    view.rows()
        .iter()
        .filter_map(|row| match &row.body {
            Body::Fire(fire) => Some((**fire).clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn a_folded_wakeup_collapses_the_previous_tick() {
    let mut events = vec![fire(CronFireKind::Task, None)];
    events.extend(turn("t1"));
    events.push(fire(CronFireKind::Loop, None));
    events.extend(turn("t2"));
    let before = view_with(events.clone());
    assert_eq!(fires(&before).len(), 2);
    events.push(fire(CronFireKind::Loop, Some(1)));
    events.extend(turn("t3"));
    let view = view_with(events);
    // 上一个 loop 触发行与它之后的行收起；普通任务的行与它的回合留着。
    let kinds: Vec<(CronFireKind, Option<u32>)> = fires(&view)
        .iter()
        .map(|fire| (fire.kind, fire.fold.as_ref().map(|fold| fold.streak)))
        .collect();
    assert_eq!(
        kinds,
        [(CronFireKind::Task, None), (CronFireKind::Loop, Some(1))]
    );
    let turns = view
        .rows()
        .iter()
        .filter(|row| matches!(row.body, Body::Turn(_)))
        .count();
    assert_eq!(turns, 2);
}
