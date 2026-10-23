//! 重试与停顿通知的投影（stall-visibility.md §3.2）：替换、清除与倒计时。
use rust_cli_kernel_api::ids::{ToolUseId, TurnId};
use rust_cli_kernel_api::model::{BlockDelta, ModelError, ModelNotice, ModelSelection};
use rust_cli_kernel_api::orchestration::QueryEvent;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{KernelEvent, SequencedEvent, TurnOutcome};
use rust_cli_kernel_api::tool::ToolProgress;
use serde_json::json;

use super::*;

fn feed(view: &mut Transcript, seq: u64, now_ms: u64, event: KernelEvent) {
    view.apply(SequencedEvent { seq, event }, now_ms);
}

fn empty() -> Transcript {
    Transcript::new(
        ModelSelection {
            provider: "fixture".into(),
            model: "scripted".into(),
            reasoning: None,
            connection: None,
        },
        PermissionMode::Default,
        crate::view::test_workspace(),
    )
}

fn retrying(delay_ms: u64) -> ModelNotice {
    ModelNotice::Retrying {
        attempt: 1,
        max_retries: 10,
        delay_ms,
        error: ModelError::Server {
            status: 500,
            message: "boom".to_owned(),
        },
    }
}

#[test]
fn the_main_notice_is_replaced_and_cleared_by_the_stream() {
    let mut view = empty();
    let turn = TurnId::new("turn_1");
    feed(
        &mut view,
        1,
        0,
        KernelEvent::TurnStarted {
            turn: turn.clone(),
            command: None,
        },
    );
    feed(
        &mut view,
        2,
        1_000,
        KernelEvent::ModelNotice {
            notice: retrying(5_000),
        },
    );
    let notice = view.notice().unwrap();
    assert_eq!(notice.at_ms, 1_000);
    // 倒计时从收到的时刻算，不小于 0。
    assert_eq!(notice.remaining_ms(3_500), 2_500);
    assert_eq!(notice.remaining_ms(9_000), 0);
    // 重发拿到响应头：清掉。
    feed(
        &mut view,
        3,
        6_000,
        KernelEvent::ModelNotice {
            notice: ModelNotice::Resumed,
        },
    );
    assert!(view.notice().is_none());
    // 停顿通知；之后的流增量清掉它。
    feed(
        &mut view,
        4,
        30_000,
        KernelEvent::ModelNotice {
            notice: ModelNotice::Stalled {
                retry_in_ms: 280_000,
            },
        },
    );
    assert_eq!(view.notice().unwrap().remaining_ms(40_000), 270_000);
    feed(
        &mut view,
        5,
        41_000,
        KernelEvent::StreamDelta {
            turn: turn.clone(),
            response: 0,
            index: 0,
            delta: BlockDelta::Text("hi".to_owned()),
        },
    );
    assert!(view.notice().is_none());
    // 回合结束也清掉。
    feed(
        &mut view,
        6,
        42_000,
        KernelEvent::ModelNotice {
            notice: retrying(1_000),
        },
    );
    feed(
        &mut view,
        7,
        43_000,
        KernelEvent::TurnFinished {
            turn,
            outcome: TurnOutcome::Failed {
                message: "boom".to_owned(),
            },
            usage: None,
        },
    );
    assert!(view.notice().is_none());
}

#[test]
fn a_subagent_notice_folds_into_its_agent_row() {
    let mut view = empty();
    let agent = ToolUseId::new("toolu_agent");
    feed(
        &mut view,
        1,
        0,
        KernelEvent::ToolStarted {
            tool_use: agent.clone(),
            name: "Agent".to_owned(),
            input: json!({"description": "Find", "prompt": "p"}),
        },
    );
    let progress = |event: QueryEvent| KernelEvent::ToolProgress {
        tool_use: agent.clone(),
        progress: ToolProgress {
            data: json!({"agentId": "agent_1", "event": event}),
        },
    };
    feed(
        &mut view,
        2,
        2_000,
        progress(QueryEvent::ModelNotice {
            notice: retrying(4_000),
        }),
    );
    let row = view.rows().iter().find_map(|row| row.tool()).unwrap();
    let notice = row.activity.as_ref().unwrap().notice.as_ref().unwrap();
    assert_eq!(notice.remaining_ms(3_000), 3_000);
    // 主线程不受子代理的通知影响。
    assert!(view.notice().is_none());
    feed(
        &mut view,
        3,
        7_000,
        progress(QueryEvent::Delta {
            message: 0,
            index: 0,
            delta: BlockDelta::Thinking("hmm".to_owned()),
        }),
    );
    let row = view.rows().iter().find_map(|row| row.tool()).unwrap();
    assert!(row.activity.as_ref().unwrap().notice.is_none());
}
