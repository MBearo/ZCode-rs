use futures_util::StreamExt;
use rust_cli_kernel_api::session_api::KernelEvent;

use super::*;

fn title(value: &str) -> KernelEvent {
    KernelEvent::TitleChanged {
        title: value.to_owned(),
    }
}

#[tokio::test]
async fn subscribers_get_the_replay_then_live_events_without_gaps() {
    let log = EventLog::new();
    assert_eq!(log.append(title("a")), 1);
    assert_eq!(log.append(title("b")), 2);
    let mut stream = log.subscribe(Some(1));
    log.append(title("c"));
    let seqs: Vec<u64> = (&mut stream).take(2).map(|event| event.seq).collect().await;
    assert_eq!(seqs, vec![2, 3]);
    assert_eq!(log.last_seq(), 3);
}

#[tokio::test]
async fn subscribing_without_a_position_starts_at_the_next_event() {
    let log = EventLog::new();
    log.append(title("old"));
    let mut stream = log.subscribe(None);
    log.append(title("new"));
    let next = stream.next().await.unwrap();
    assert_eq!(next.seq, 2);
}

use futures_util::FutureExt;
use rust_cli_kernel_api::ids::{ToolUseId, TurnId};
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::orchestration::QueryEvent;
use rust_cli_kernel_api::tool::{ToolOutput, ToolProgress};
use serde_json::json;

fn delta(response: u32, index: usize, text: &str) -> KernelEvent {
    KernelEvent::StreamDelta {
        turn: TurnId::new("t1"),
        response,
        index,
        delta: BlockDelta::Text(text.to_owned()),
    }
}

fn tool(id: &str) -> ToolUseId {
    ToolUseId::new(id)
}

fn output(id: &str, text: &str) -> KernelEvent {
    KernelEvent::ToolProgress {
        tool_use: tool(id),
        progress: ToolProgress {
            data: json!({"type": "bash_progress", "output": text, "totalBytes": text.len()}),
        },
    }
}

fn agent(id: &str, event: QueryEvent) -> KernelEvent {
    KernelEvent::ToolProgress {
        tool_use: tool(id),
        progress: ToolProgress {
            data: json!({"agentId": "agent_1", "event": event}),
        },
    }
}

fn child_delta(text: &str) -> QueryEvent {
    QueryEvent::Delta {
        message: 0,
        index: 1,
        delta: BlockDelta::Thinking(text.to_owned()),
    }
}

/// 补发的事件（订阅时已经在日志里的部分）。
fn replay(log: &EventLog, after: u64) -> Vec<SequencedEvent> {
    let mut stream = log.subscribe(Some(after));
    let mut events = Vec::new();
    while let Some(Some(event)) = stream.next().now_or_never() {
        events.push(event);
    }
    events
}

#[test]
fn a_block_of_deltas_replays_as_one_delta_with_the_rest_of_the_text() {
    let log = EventLog::new();
    for piece in ["a", "b", "c"] {
        log.append(delta(0, 0, piece));
    }
    log.append(title("t"));
    let all = replay(&log, 0);
    assert_eq!(all.len(), 2);
    assert_eq!((all[0].seq, &all[0].event), (3, &delta(0, 0, "abc")));
    assert_eq!(all[1].seq, 4);
    let rest = replay(&log, 1);
    assert_eq!((rest[0].seq, &rest[0].event), (3, &delta(0, 0, "bc")));
    assert_eq!(replay(&log, 3).len(), 1);
    // 不同的块、不同的响应各成一段，顺序不变。
    log.append(delta(0, 1, "x"));
    log.append(delta(1, 0, "y"));
    log.append(delta(1, 0, "z"));
    let tail: Vec<KernelEvent> = replay(&log, 4).into_iter().map(|e| e.event).collect();
    assert_eq!(tail, vec![delta(0, 1, "x"), delta(1, 0, "yz")]);
}

#[test]
fn only_the_latest_output_progress_of_a_tool_is_replayed() {
    let log = EventLog::new();
    log.append(output("bash_1", "one"));
    log.append(output("bash_2", "other"));
    log.append(output("bash_1", "one two"));
    log.append(KernelEvent::ToolDone {
        tool_use: tool("bash_1"),
        output: ToolOutput {
            content: Vec::new(),
            is_error: false,
            display: serde_json::Value::Null,
        },
    });
    let seqs: Vec<u64> = replay(&log, 0).iter().map(|event| event.seq).collect();
    assert_eq!(seqs, vec![2, 3, 4]);
    assert_eq!(replay(&log, 2)[0].event, output("bash_1", "one two"));
}

#[tokio::test]
async fn subagent_tool_progress_is_broadcast_but_not_kept() {
    let log = EventLog::new();
    let mut live = log.subscribe(None);
    let progress = agent(
        "agent_tool",
        QueryEvent::ToolProgress {
            tool_use: tool("child_bash"),
            progress: ToolProgress {
                data: json!({"type": "bash_progress", "output": "x"}),
            },
        },
    );
    log.append(progress.clone());
    assert_eq!(live.next().await.unwrap().event, progress);
    assert!(replay(&log, 0).is_empty());
    assert_eq!(log.last_seq(), 1);
}

#[test]
fn subagent_deltas_merge_like_stream_deltas() {
    let log = EventLog::new();
    log.append(agent("agent_tool", child_delta("think")));
    log.append(agent("agent_tool", child_delta("ing")));
    log.append(agent(
        "agent_tool",
        QueryEvent::Delta {
            message: 0,
            index: 2,
            delta: BlockDelta::Text("x".to_owned()),
        },
    ));
    let all = replay(&log, 0);
    assert_eq!(all.len(), 2);
    assert_eq!(
        (all[0].seq, &all[0].event),
        (2, &agent("agent_tool", child_delta("thinking")))
    );
    assert_eq!(
        replay(&log, 1)[0].event,
        agent("agent_tool", child_delta("ing"))
    );
}

/// 修复（R1-4）：以前日志保存每个增量与每条进度，长会话里只增不减。
#[test]
fn the_log_does_not_grow_with_deltas_and_progress() {
    let retained = |blocks: usize| {
        let log = EventLog::new();
        let tail = "o".repeat(2_000);
        for block in 0..blocks {
            for piece in 0..1_000 {
                log.append(delta(0, block, "w"));
                if piece % 5 == 0 {
                    log.append(output("bash_1", &tail));
                }
            }
            log.append(title("between blocks"));
        }
        log.retained()
    };
    let small = retained(10);
    let large = retained(20);
    // 12_010 个与 24_020 个事件：存的条目只跟块数与非瞬时事件走，流式文字每块一份。
    assert_eq!(small.text_bytes, 10_000);
    assert_eq!(large.text_bytes, 20_000);
    assert!(small.entries <= 300, "{small:?}");
    assert!(large.entries <= 300, "{large:?}");
}
