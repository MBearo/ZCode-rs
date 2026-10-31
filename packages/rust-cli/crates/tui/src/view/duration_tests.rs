//! 回合结束那一行进视图（docs/specs/rust-cli/tui-display.md §2.7，修复线 F118）：`TurnDuration` 折成一行，排在这个回合的
//! 错误行之后；它不是还在变化的行。
use rust_cli_kernel_api::ids::TurnId;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOutcome};
use rust_cli_kernel_api::testing::selection;
use rust_cli_kernel_api::turn_duration::TurnDuration;

use super::*;

fn folded(events: Vec<KernelEvent>) -> Transcript {
    let mut view = Transcript::new(selection(), PermissionMode::Default, test_workspace());
    for (seq, event) in (1..).zip(events) {
        view.apply(
            SequencedEvent {
                seq,
                revision: 0,
                event,
            },
            seq * 1000,
        );
    }
    view
}

fn line() -> TurnDuration {
    TurnDuration {
        turn: TurnId::new("turn_1"),
        duration_ms: 62_000,
        finished_at_ms: 0,
        utc_offset_minutes: 0,
        waiting_agents: 0,
    }
}

#[test]
fn the_line_comes_after_the_error_of_its_turn() {
    let turn = TurnId::new("turn_1");
    let view = folded(vec![
        KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: None,
        },
        KernelEvent::TurnFinished {
            turn,
            outcome: TurnOutcome::Failed {
                message: "bad request".into(),
            },
            usage: None,
        },
        KernelEvent::TurnDuration(line()),
    ]);
    let bodies: Vec<&Body> = view.rows().iter().map(|row| &row.body).collect();
    assert_eq!(
        bodies,
        [
            &Body::Turn(TurnState::Failed),
            &Body::Error("bad request".into()),
            &Body::Duration(Box::new(line())),
        ]
    );
    assert!(!view.running());
    assert!(view.rows().iter().all(|row| !row.live()));
}
