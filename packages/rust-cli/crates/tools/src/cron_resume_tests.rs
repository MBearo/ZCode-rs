//! resume 复原会话任务（docs/specs/rust-cli/tools-more.md §18.2）。

use rust_cli_kernel_api::cron::CronSignals;
use rust_cli_kernel_api::ids::{MessageId, SessionId, ToolUseId};
use rust_cli_kernel_api::message::{
    AssistantMessage, ContentBlock, Message, ToolResultContent, UserMessage, UserOrigin,
};
use serde_json::{Value, json};

use crate::cron_board::tests::TokioClock;
use crate::cron_board::{CronBoard, JobKind};
use crate::cron_expr::RECURRING_MAX_AGE_MS;

/// TokioClock 的起点 2026-10-17T10:07:30Z。
const START: u64 = 1_792_231_650_000;
const MINUTE: u64 = 60_000;

fn owner() -> SessionId {
    SessionId::new("sess_1")
}

/// Node 格式的消息 ID：`msg_<36 进制毫秒>_<uuid>`。
fn message_id(at: u64, n: u32) -> MessageId {
    let mut digits = Vec::new();
    let mut value = at;
    while value > 0 {
        digits.push(char::from_digit(u32::try_from(value % 36).unwrap(), 36).unwrap());
        value /= 36;
    }
    let base36: String = digits.into_iter().rev().collect();
    MessageId::new(format!("msg_{base36}_00000000-0000-4000-8000-{n:012}"))
}

fn call(at: u64, n: u32, tool: &str, input: Value) -> Message {
    Message::Assistant(AssistantMessage {
        id: message_id(at, n),
        content: vec![ContentBlock::ToolUse {
            id: ToolUseId::new(format!("toolu_{n}")),
            name: tool.to_owned(),
            input,
        }],
        model: "m".to_owned(),
        stop_reason: None,
        usage: None,
    })
}

fn result(at: u64, n: u32, text: &str, is_error: bool) -> Message {
    Message::User(UserMessage {
        id: message_id(at + 1, n + 100),
        content: vec![ContentBlock::ToolResult {
            tool_use_id: ToolUseId::new(format!("toolu_{n}")),
            content: vec![ToolResultContent::Text {
                text: text.to_owned(),
            }],
            is_error,
        }],
        origin: UserOrigin::ToolResult {
            assistant: message_id(at, n),
        },
    })
}

fn created(at: u64, n: u32, cron: &str, prompt: &str, text: &str) -> [Message; 2] {
    [
        call(at, n, "CronCreate", json!({"cron": cron, "prompt": prompt})),
        result(at, n, text, false),
    ]
}

const SESSION_ONLY: &str = "Session-only (not written to disk, dies when ZCode exits)";

fn recurring(id: &str) -> String {
    format!(
        "Scheduled recurring job {id} (Every 5 minutes). {SESSION_ONLY}. Auto-expires after 7 days. Use CronDelete to cancel sooner."
    )
}

fn one_shot(id: &str) -> String {
    format!(
        "Scheduled one-shot task {id} (12 10 * * *). {SESSION_ONLY}. It will fire once then auto-delete."
    )
}

#[tokio::test(start_paused = true)]
async fn resume_rebuilds_session_tasks_from_the_history() {
    let board = CronBoard::new(TokioClock::new(), None);
    let earlier = START - 2 * MINUTE;
    let mut history = Vec::new();
    // 复原：循环任务、还没到点的一次性任务（10:12）。
    history.extend(created(
        earlier,
        1,
        "*/5 * * * *",
        "check",
        &recurring("1a2b3c4d"),
    ));
    history.extend(created(
        earlier,
        2,
        "12 10 * * *",
        "ping",
        &one_shot("2b3c4d5e"),
    ));
    // 不复原：已过点的一次性任务（09:47 建、10:00 该触发）、持久任务、被删除的、失败的、满 7 天的循环任务。
    history.extend(created(
        START - 20 * MINUTE,
        3,
        "0 10 * * *",
        "past",
        &one_shot("3c4d5e6f"),
    ));
    history.extend(created(
        earlier,
        4,
        "*/5 * * * *",
        "durable",
        "Scheduled recurring job 4d5e6f70 (Every 5 minutes). Persisted to .zcode/scheduled_tasks.json. Auto-expires after 7 days. Use CronDelete to cancel sooner.",
    ));
    history.extend(created(
        earlier,
        5,
        "*/5 * * * *",
        "deleted",
        &recurring("5e6f7081"),
    ));
    history.push(call(earlier, 6, "CronDelete", json!({"id": "5e6f7081"})));
    history.push(call(
        earlier,
        7,
        "CronCreate",
        json!({"cron": "bad", "prompt": "x"}),
    ));
    history.push(result(
        earlier,
        7,
        "<tool_use_error>Invalid cron expression</tool_use_error>",
        true,
    ));
    let old = START - RECURRING_MAX_AGE_MS - MINUTE;
    history.extend(created(
        old,
        8,
        "*/5 * * * *",
        "stale",
        &recurring("6f708192"),
    ));
    board.resume(&owner(), &history).await;
    let jobs = board.session_jobs(&owner());
    let restored: Vec<(&str, &str, bool)> = jobs
        .iter()
        .map(|job| (job.id.as_str(), job.prompt.as_str(), job.recurring))
        .collect();
    assert_eq!(
        restored,
        [("1a2b3c4d", "check", true), ("2b3c4d5e", "ping", false)]
    );
    assert!(jobs.iter().all(|job| job.kind == JobKind::Cron));
    // 循环任务的下次触发从现在算（上次触发记为现在），7 天到期仍按创建时间。
    assert!(jobs[0].next_fire.is_some_and(|at| at > START));
    assert_eq!(jobs[0].created_at, earlier);
    // 再复原一次不重复。
    board.resume(&owner(), &history).await;
    assert_eq!(board.session_count(&owner()), 2);
    // 到点照常触发。
    tokio::time::sleep(std::time::Duration::from_millis(5 * MINUTE)).await;
    let prompts: Vec<String> = board
        .take_due(&owner())
        .await
        .into_iter()
        .map(|fire| fire.prompt)
        .collect();
    assert!(prompts.contains(&"ping".to_owned()), "{prompts:?}");
}

/// 消息 ID 不是 Node 格式时没有创建时间：循环任务按现在，一次性任务跳过。
#[tokio::test(start_paused = true)]
async fn ids_without_a_time_keep_recurring_tasks_only() {
    let board = CronBoard::new(TokioClock::new(), None);
    let history = [
        Message::Assistant(AssistantMessage {
            id: MessageId::new("m1"),
            content: vec![
                ContentBlock::ToolUse {
                    id: ToolUseId::new("toolu_1"),
                    name: "CronCreate".to_owned(),
                    input: json!({"cron": "*/5 * * * *", "prompt": "check"}),
                },
                ContentBlock::ToolUse {
                    id: ToolUseId::new("toolu_2"),
                    name: "CronCreate".to_owned(),
                    input: json!({"cron": "12 10 * * *", "prompt": "ping"}),
                },
            ],
            model: "m".to_owned(),
            stop_reason: None,
            usage: None,
        }),
        Message::User(UserMessage {
            id: MessageId::new("u1"),
            content: vec![
                ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new("toolu_1"),
                    content: vec![ToolResultContent::Text {
                        text: recurring("1a2b3c4d"),
                    }],
                    is_error: false,
                },
                ContentBlock::ToolResult {
                    tool_use_id: ToolUseId::new("toolu_2"),
                    content: vec![ToolResultContent::Text {
                        text: one_shot("2b3c4d5e"),
                    }],
                    is_error: false,
                },
            ],
            origin: UserOrigin::ToolResult {
                assistant: MessageId::new("m1"),
            },
        }),
    ];
    board.resume(&owner(), &history).await;
    let jobs = board.session_jobs(&owner());
    assert_eq!(jobs.len(), 1);
    assert_eq!(
        (jobs[0].id.as_str(), jobs[0].created_at),
        ("1a2b3c4d", START)
    );
}
