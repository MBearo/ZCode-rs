//! 投影的纯函数测试：合成的内核事件 → App 的行与状态。

use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{
    CommandId, EnvironmentId, InteractionId, MessageId, SessionId, ToolUseId, TraceId, TurnId,
};
use rust_cli_kernel_api::message::{AssistantMessage, Message, UserMessage, UserOrigin};
use rust_cli_kernel_api::model::BlockDelta;
use rust_cli_kernel_api::orchestration::InteractionRequest;
use rust_cli_kernel_api::store::{SessionKind, SessionMeta};
use rust_cli_kernel_api::testing::selection;
use serde_json::json;

use super::*;

fn view(messages: Vec<Message>) -> SessionView {
    SessionView {
        meta: SessionMeta {
            id: SessionId::new("session_1"),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/w"),
            },
            title: String::new(),
            trace: TraceId::new("trace"),
            created_at_ms: 1000,
        },
        messages,
        selection: selection(),
        mode: PermissionMode::Default,
        plan: false,
        pending: Vec::new(),
        queue: Vec::new(),
        last_seq: 0,
        revision: 0,
        running: false,
        background: Vec::new(),
        peer_held: Vec::new(),
        context_usage: None,
        context_notice: Default::default(),
    }
}

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin: UserOrigin::Human,
    })
}

fn assistant(id: &str, content: Vec<ContentBlock>) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content,
        model: "m".into(),
        stop_reason: None,
        usage: None,
    })
}

/// 按顺序喂事件（seq 从 1 起，revision 照会话 actor 的规则：状态变化加一）。
struct Feed {
    projection: Projection,
    seq: u64,
    revision: u64,
}

impl Feed {
    fn new() -> Self {
        Self {
            projection: Projection::from_view(
                &view(Vec::new()),
                Followup::Queue,
                vec!["high".into()],
            ),
            seq: 0,
            revision: 0,
        }
    }

    fn apply(&mut self, event: KernelEvent) -> Vec<Value> {
        self.seq += 1;
        if !matches!(
            event,
            KernelEvent::StreamDelta { .. } | KernelEvent::ContextUsage { .. }
        ) {
            self.revision += 1;
        }
        self.projection.apply(
            &SequencedEvent {
                seq: self.seq,
                event,
                revision: self.revision,
            },
            2000 + self.seq,
        )
    }
}

fn ops(deltas: &[Value]) -> Vec<String> {
    deltas
        .iter()
        .map(|delta| match delta["op"].as_str().unwrap() {
            "state.updated" => {
                let mut keys: Vec<&str> = delta["patch"]
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect();
                keys.sort_unstable();
                format!("state({})", keys.join(","))
            }
            op if delta.get("row").is_some() => {
                format!("{op}({})", delta["row"]["kind"].as_str().unwrap())
            }
            op => op.to_owned(),
        })
        .collect()
}

#[test]
fn a_text_turn_becomes_rows_and_control_states() {
    let mut feed = Feed::new();
    let started = feed.apply(KernelEvent::TurnStarted {
        turn: TurnId::new("turn_1"),
        command: Some(CommandId::new("c1")),
    });
    assert_eq!(
        ops(&started),
        [
            "row.appended(turnHeader)",
            "state(availability,control,inputRouting,revision)"
        ]
    );
    let patch = &started[1]["patch"];
    assert_eq!(patch["control"]["phase"], "running");
    assert_eq!(patch["control"]["canStop"], true);
    assert_eq!(
        patch["control"]["activeWorks"][0]["foregroundExecutionId"],
        "runtime_command_1"
    );
    assert_eq!(patch["inputRouting"]["mode"], "enqueue");
    let input = feed.apply(KernelEvent::MessageDone {
        message: user("msg_u1", "hello"),
    });
    assert_eq!(input[0]["row"]["text"], "hello");
    assert_eq!(input[0]["row"]["sourceCommandId"], "c1");
    let delta = |text: &str| KernelEvent::StreamDelta {
        turn: TurnId::new("turn_1"),
        response: 0,
        index: 0,
        delta: BlockDelta::Text(text.into()),
    };
    assert_eq!(
        ops(&feed.apply(delta("hi "))),
        ["row.appended(assistantText)", "row.delta"]
    );
    assert_eq!(ops(&feed.apply(delta("there"))), ["row.delta"]);
    let done = feed.apply(KernelEvent::MessageDone {
        message: assistant(
            "msg_a1",
            vec![ContentBlock::Text {
                text: "hi there".into(),
            }],
        ),
    });
    assert_eq!(done[0]["row"]["state"], "complete");
    assert_eq!(done[0]["row"]["text"], "hi there");
    let finished = feed.apply(KernelEvent::TurnFinished {
        turn: TurnId::new("turn_1"),
        outcome: TurnOutcome::Success,
        usage: Some(Usage {
            input_tokens: 10,
            output_tokens: 3,
            ..Usage::default()
        }),
    });
    assert_eq!(
        ops(&finished),
        [
            "row.upserted(turnHeader)",
            "state(availability,control,inputRouting,revision,usage)"
        ]
    );
    assert_eq!(finished[0]["row"]["state"], "completedSuccess");
    assert_eq!(finished[1]["patch"]["control"]["phase"], "completedSuccess");
    assert_eq!(
        finished[1]["patch"]["usage"]["cumulative"]["outputTokens"],
        3
    );
    let snapshot = feed.projection.snapshot(9, "epoch-1");
    let kinds: Vec<&str> = snapshot["rows"]["window"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| row["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["turnHeader", "userInput", "assistantText"]);
    assert_eq!(snapshot["rows"]["firstRowId"], 1);
    assert_eq!(snapshot["seq"], 9);
    assert_eq!(snapshot["revision"], feed.revision);
    assert_eq!(snapshot["config"]["followupMode"], "queue");
    assert_eq!(feed.projection.last_reply().as_deref(), Some("hi there"));
}

#[test]
fn a_permission_ask_marks_the_tool_and_lists_the_interaction() {
    let mut feed = Feed::new();
    feed.apply(KernelEvent::TurnStarted {
        turn: TurnId::new("turn_1"),
        command: None,
    });
    let tool = ToolUseId::new("toolu_1");
    feed.apply(KernelEvent::MessageDone {
        message: assistant(
            "msg_a1",
            vec![ContentBlock::ToolUse {
                id: tool.clone(),
                name: "Write".into(),
                input: json!({"file_path": "/w/a.txt"}),
            }],
        ),
    });
    let asked = feed.apply(KernelEvent::InteractionAsked {
        interaction: InteractionId::new("int_1"),
        request: InteractionRequest::Permission {
            tool_use: tool.clone(),
            tool: "Write".into(),
            input: json!({"file_path": "/w/a.txt"}),
            message: "Write a.txt?".into(),
            suggestions: Vec::new(),
            asked_by: None,
            machine: None,
        },
    });
    assert_eq!(asked[0]["row"]["status"], "pendingApproval");
    assert_eq!(asked[0]["row"]["approvalInteractionId"], "int_1");
    let pending = &asked[1]["patch"]["pendingInteractions"][0];
    assert_eq!(pending["interactionId"], "int_1");
    assert_eq!(pending["anchorRowId"], asked[0]["row"]["rowId"]);
    assert_eq!(pending["payload"]["toolCallId"], "toolu_1");
    assert_eq!(
        pending["payload"]["fullAccessOption"]["optionId"],
        "fullAccess"
    );
    assert_eq!(pending["payload"]["options"].as_array().unwrap().len(), 3);
    assert_eq!(feed.projection.pending_permissions(), 1);
    let resolved = feed.apply(KernelEvent::InteractionResolved {
        interaction: InteractionId::new("int_1"),
    });
    assert_eq!(resolved[0]["row"]["status"], "running");
    assert_eq!(resolved[1]["patch"]["pendingInteractions"], json!([]));
    let done = feed.apply(KernelEvent::ToolDone {
        tool_use: tool,
        output: rust_cli_kernel_api::tool::ToolOutput {
            content: vec![ToolResultContent::Text { text: "ok".into() }],
            is_error: false,
            display: Value::Null,
        },
    });
    assert_eq!(done[0]["row"]["status"], "success");
    assert_eq!(done[0]["row"]["output"]["text"], "ok");
}

#[test]
fn an_interrupted_turn_cancels_running_tools() {
    let mut feed = Feed::new();
    feed.apply(KernelEvent::TurnStarted {
        turn: TurnId::new("turn_1"),
        command: None,
    });
    feed.apply(KernelEvent::ToolStarted {
        tool_use: ToolUseId::new("toolu_1"),
        name: "Bash".into(),
        input: json!({"command": "sleep 60"}),
    });
    let finished = feed.apply(KernelEvent::TurnFinished {
        turn: TurnId::new("turn_1"),
        outcome: TurnOutcome::Interrupted,
        usage: None,
    });
    let rows: Vec<(&str, &str)> = finished
        .iter()
        .filter_map(|delta| {
            let row = delta.get("row")?;
            Some((
                row["kind"].as_str()?,
                row["status"].as_str().or(row["state"].as_str())?,
            ))
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("toolCall", "cancelled"),
            ("turnHeader", "completedInterrupted")
        ]
    );
    assert_eq!(feed.projection.phase(), Phase::CompletedInterrupted);
}

#[test]
fn a_cold_view_rebuilds_the_rows_of_finished_turns() {
    let projection = Projection::from_view(
        &view(vec![
            user("msg_u1", "first"),
            assistant("msg_a1", vec![ContentBlock::Text { text: "one".into() }]),
            user("msg_u2", "second"),
            assistant("msg_a2", vec![ContentBlock::Text { text: "two".into() }]),
        ]),
        Followup::Guide,
        Vec::new(),
    );
    let snapshot = projection.snapshot(0, "epoch");
    let rows: Vec<String> = snapshot["rows"]["window"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            format!(
                "{}:{}",
                row["kind"].as_str().unwrap(),
                row["text"].as_str().or(row["state"].as_str()).unwrap_or("")
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            "turnHeader:completedSuccess",
            "userInput:first",
            "assistantText:one",
            "turnHeader:completedSuccess",
            "userInput:second",
            "assistantText:two",
        ]
    );
    assert_eq!(snapshot["control"]["phase"], "completedSuccess");
    assert_eq!(snapshot["inputRouting"]["mode"], "startNow");
    assert_eq!(snapshot["config"]["followupMode"], "guide");
}

/// revision 只在事件带来的值变了时才进 patch；流式增量不带 patch。
#[test]
fn the_revision_comes_from_the_events_only() {
    let mut projection = Projection::from_view(&view(Vec::new()), Followup::Queue, Vec::new());
    let event = |seq, revision, event| SequencedEvent {
        seq,
        event,
        revision,
    };
    let deltas = projection.apply(
        &event(1, 4, KernelEvent::TitleChanged { title: "t".into() }),
        0,
    );
    assert_eq!(deltas[0]["patch"]["revision"], 4);
    assert_eq!(deltas[0]["patch"]["meta"]["title"], "t");
    let deltas = projection.apply(
        &event(
            2,
            4,
            KernelEvent::ContextUsage {
                used_tokens: 5,
                max_tokens: 10,
            },
        ),
        0,
    );
    assert!(deltas[0]["patch"].get("revision").is_none());
    assert_eq!(projection.revision(), 4);
    let changed = projection.set_followup(Followup::Guide);
    assert_eq!(changed["patch"]["config"]["followupMode"], "guide");
}

/// 对拍找到的三处（与 Node 相同）：拒绝了的工具调用投成 cancelled；中断后补的那条消息不出行；空闲时提交的输入不投成排队项。
#[test]
fn denied_tools_interrupt_markers_and_immediate_inputs_follow_node() {
    let mut feed = Feed::new();
    feed.projection.remember_input(
        "c1",
        InputMeta {
            client: "k".into(),
            origin: None,
            text: "hi".into(),
            selection: None,
            mode: None,
            plan: None,
            admitted_at: 0,
            requested: "startNow",
            admitted: "startNow",
        },
    );
    let queued = feed.apply(KernelEvent::QueueChanged {
        items: vec![QueuedInput {
            command: CommandId::new("c1"),
            text: "hi".into(),
            delivery: rust_cli_kernel_api::session_api::Delivery::Queue,
        }],
    });
    assert!(
        queued
            .iter()
            .all(|delta| delta["patch"].get("queue").is_none())
    );
    feed.apply(KernelEvent::TurnStarted {
        turn: TurnId::new("turn_1"),
        command: Some(CommandId::new("c1")),
    });
    feed.apply(KernelEvent::ToolStarted {
        tool_use: ToolUseId::new("toolu_1"),
        name: "Bash".into(),
        input: json!({"command": "rm -rf x"}),
    });
    feed.projection.deny("toolu_1");
    let done = feed.apply(KernelEvent::ToolDone {
        tool_use: ToolUseId::new("toolu_1"),
        output: rust_cli_kernel_api::tool::ToolOutput {
            content: vec![ToolResultContent::Text {
                text: "denied".into(),
            }],
            is_error: true,
            display: Value::Null,
        },
    });
    assert_eq!(done[0]["row"]["status"], "cancelled");
    let marker = feed.apply(KernelEvent::MessageDone {
        message: user("msg_i", rust_cli_kernel_api::user_input::INTERRUPT_TEXTS[0]),
    });
    assert!(marker.iter().all(|delta| delta.get("row").is_none()));
}
