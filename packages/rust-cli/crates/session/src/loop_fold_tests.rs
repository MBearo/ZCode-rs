//! noop 折叠的判定与文字（tools-more.md §18.3）。

use rust_cli_kernel_api::ids::{MessageId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, AttachmentMessage, ContentBlock, Message, SystemMessage, ToolResultContent,
    UserMessage, UserOrigin,
};
use serde_json::json;

use super::*;

fn prompt(id: &str) -> Message {
    meta(id, CRON_SOURCE)
}

fn meta(id: &str, source: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text {
            text: "x".to_owned(),
        }],
        origin: UserOrigin::Meta {
            source: source.to_owned(),
        },
    })
}

fn wakeup(id: &str, tool: &str, noop: bool) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new(tool),
            name: "ScheduleWakeup".to_owned(),
            input: json!({"delaySeconds": 600, "reason": "r", "prompt": "/loop x", "noop": noop}),
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn result(id: &str, tool: &str, text: &str, is_error: bool) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(tool),
            content: vec![ToolResultContent::Text {
                text: text.to_owned(),
            }],
            is_error,
        }],
        origin: UserOrigin::ToolResult {
            assistant: MessageId::new("a"),
        },
    })
}

fn anchored(at_ms: u64) -> FoldState {
    let mut state = FoldState::default();
    state.fired(MessageId::new("p1"), None, at_ms);
    state
}

fn tick(noop: bool) -> Vec<Message> {
    vec![
        prompt("p1"),
        wakeup("a1", "t1", noop),
        result("r1", "t1", "Next wakeup scheduled", false),
    ]
}

#[test]
fn a_quiet_tick_folds_and_counts_the_streak() {
    let state = anchored(5_000);
    let fold = state.analyze(&tick(true)).unwrap();
    assert_eq!(
        fold,
        Fold {
            streak: 1,
            since_ms: 5_000
        }
    );
    // 下一个锚点接着数，开始时间不变。
    let mut state = state;
    state.fired(MessageId::new("p2"), Some(fold), 9_000);
    let mut history = tick(true);
    history.extend([
        meta("fold", FOLD_SOURCE),
        prompt("p2"),
        // skill 正文等不带来源的 meta 不打断。
        meta("s", "skill"),
        wakeup("a2", "t2", true),
        result("r2", "t2", "ok", false),
    ]);
    assert_eq!(
        state.analyze(&history),
        Some(Fold {
            streak: 2,
            since_ms: 5_000
        })
    );
}

#[test]
fn work_or_foreign_input_vetoes_the_fold() {
    let state = anchored(0);
    assert_eq!(FoldState::default().analyze(&tick(true)), None, "no anchor");
    assert_eq!(state.analyze(&tick(false)), None, "the model reported work");
    let vetoes: Vec<Message> = vec![
        Message::User(UserMessage {
            id: MessageId::new("h"),
            content: vec![ContentBlock::Text {
                text: "hi".to_owned(),
            }],
            origin: UserOrigin::Human,
        }),
        meta("n", NOTICE_SOURCE),
        prompt("other"),
        Message::System(SystemMessage::Interrupted {
            id: MessageId::new("i"),
        }),
        Message::Attachment(AttachmentMessage {
            id: MessageId::new("q"),
            source: "queued_command".to_owned(),
            text: "x".to_owned(),
            data: serde_json::Value::Null,
        }),
        result("r9", "t9", "orphan", false),
        result(
            "r1b",
            "t1",
            "The user doesn't want to proceed with this tool use. The tool use was rejected",
            true,
        ),
    ];
    for veto in vetoes {
        let mut history = tick(true);
        history.push(veto.clone());
        assert_eq!(state.analyze(&history), None, "{veto:?}");
    }
    // 锚点之前到上一个 loop 触发之间有别的定时任务。
    let mut state = FoldState::default();
    state.fired(MessageId::new("p0"), None, 0);
    state.fired(MessageId::new("p1"), None, 0);
    let mut history = vec![prompt("p0"), prompt("cron"), wakeup("a0", "t0", true)];
    history.push(result("r0", "t0", "ok", false));
    history.extend(tick(true));
    assert_eq!(state.analyze(&history), None);
}

#[test]
fn fold_texts_are_verbatim() {
    assert_eq!(
        note(1),
        "[1 prior /loop wakeup found nothing actionable; loop is healthy.]"
    );
    assert_eq!(
        note(3),
        "[3 prior /loop wakeups found nothing actionable; loop is healthy.]"
    );
    // 2026-10-18T15:04:00Z → `Oct 18 3:04pm`；本地偏移 +8 小时 → `Oct 18 11:04pm`；午夜是 12 点。
    let at = 1_792_335_840_000;
    assert_eq!(label(at, 0), "Oct 18 3:04pm");
    assert_eq!(label(at, 8 * 60), "Oct 18 11:04pm");
    assert_eq!(label(at + 9 * 3_600_000, 0), "Oct 19 12:04am");
    assert_eq!(label(1_000, 0), "Jan 1 12:00am");
}
