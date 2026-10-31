use serde_json::json;

use super::*;
use crate::agents::{AgentSignals, NoAgentSignals};
use crate::ids::SessionId;
use crate::session_api::KernelEvent;

fn line(waiting_agents: u32) -> TurnDuration {
    TurnDuration {
        turn: TurnId::new("turn-1"),
        duration_ms: 62_000,
        finished_at_ms: 1_700_000_000_000,
        utc_offset_minutes: 480,
        waiting_agents,
    }
}

#[test]
fn the_event_is_flat_and_leaves_out_zero_waiting_agents() {
    let event = KernelEvent::TurnDuration(line(0));
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(
        value,
        json!({
            "type": "turn_duration",
            "turn": "turn-1",
            "duration_ms": 62_000,
            "finished_at_ms": 1_700_000_000_000_u64,
            "utc_offset_minutes": 480,
        })
    );
    assert_eq!(serde_json::from_value::<KernelEvent>(value).unwrap(), event);
    let waiting = serde_json::to_value(KernelEvent::TurnDuration(line(2))).unwrap();
    assert_eq!(waiting["waiting_agents"], 2);
}

#[test]
fn a_line_without_the_offset_and_the_count_reads_as_utc_and_no_waiting() {
    let value = json!({
        "type": "turn_duration",
        "turn": "turn-1",
        "duration_ms": 5,
        "finished_at_ms": 7,
    });
    let KernelEvent::TurnDuration(line) = serde_json::from_value(value).unwrap() else {
        panic!("not a turn duration");
    };
    assert_eq!((line.utc_offset_minutes, line.waiting_agents), (0, 0));
}

#[tokio::test]
async fn without_agents_no_background_agent_is_waited_for() {
    assert!(
        NoAgentSignals
            .background_agents(&SessionId::new("s"))
            .await
            .is_empty()
    );
}
