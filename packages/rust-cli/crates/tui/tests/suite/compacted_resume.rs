//! 重新打开压缩过两次的会话（docs/specs/rust-cli/compact.md §5.1，工作线 F102）：界面有完整的历史，每个压缩点一行
//! “对话已压缩”，摘要不显示；ctrl+o 的详细视图里也有压缩前的内容；下一个请求仍从最后一个压缩点开始。
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyModifiers};
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, OwnerEpoch, SessionId, TraceId};
use rust_cli_kernel_api::message::{
    AssistantMessage, CompactBoundary, CompactTrigger, ContentBlock, Message, PreservedSegment,
    StopReason, SystemMessage, UserMessage, UserOrigin,
};
use rust_cli_kernel_api::store::{Fact, FactBatch, SessionKind, SessionMeta, SessionStore};
use rust_cli_kernel_api::testing::{MemoryStore, text_response};

use super::support::{GatedModel, Setup, request_text, with_ui};

const SESSION: &str = "sess_compacted";
const COMPACTED: &str = "Conversation compacted (ctrl+o for history)";

fn user(id: &str, text: &str) -> Message {
    Message::User(UserMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        origin: UserOrigin::Human,
    })
}

fn reply(id: &str, text: &str) -> Message {
    Message::Assistant(AssistantMessage {
        id: MessageId::new(id),
        content: vec![ContentBlock::Text { text: text.into() }],
        model: "scripted".into(),
        stop_reason: Some(StopReason::EndTurn),
        usage: None,
    })
}

/// 边界与摘要；`kept` 是保留段的首尾（同一条消息）。
fn compaction(k: usize, kept: Option<&str>) -> Vec<Message> {
    let summary = MessageId::new(format!("msg_s{k}"));
    vec![
        Message::System(SystemMessage::CompactBoundary(CompactBoundary {
            id: MessageId::new(format!("msg_b{k}")),
            trigger: CompactTrigger::Manual,
            pre_tokens: 100,
            summary: summary.clone(),
            preserved: kept.map(|id| PreservedSegment {
                head: MessageId::new(id),
                anchor: summary.clone(),
                tail: MessageId::new(id),
            }),
            discovered_tools: Vec::new(),
        })),
        Message::User(UserMessage {
            id: summary,
            content: vec![ContentBlock::Text {
                text: format!("SUMMARY-TEXT-{k}"),
            }],
            origin: UserOrigin::CompactSummary,
        }),
    ]
}

/// 两轮、压缩（没有保留段）、两轮、压缩（保留最后一条回复）、一轮。
async fn seeded() -> Arc<MemoryStore> {
    let store = Arc::new(MemoryStore::default());
    store.split_at_compaction();
    let id = SessionId::new(SESSION);
    store
        .create(SessionMeta {
            id: id.clone(),
            kind: SessionKind::Interactive,
            parent: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/Users/dev/project"),
            },
            title: "Compacted work".into(),
            trace: TraceId::new(SESSION),
            created_at_ms: 1_000_000,
        })
        .await
        .unwrap();
    let mut messages = vec![
        user("msg_u1", "first question"),
        reply("msg_a1", "FIRST-ANSWER"),
        user("msg_u2", "second question"),
        reply("msg_a2", "SECOND-ANSWER"),
    ];
    messages.extend(compaction(1, None));
    messages.extend([
        user("msg_u3", "third question"),
        reply("msg_a3", "THIRD-ANSWER"),
        user("msg_u4", "fourth question"),
        reply("msg_a4", "FOURTH-ANSWER"),
    ]);
    messages.extend(compaction(2, Some("msg_a4")));
    messages.extend([
        user("msg_u5", "fifth question"),
        reply("msg_a5", "FIFTH-ANSWER"),
    ]);
    let facts = messages
        .into_iter()
        .map(|message| Fact::MessageAppended { message })
        .collect();
    store
        .commit(
            &id,
            FactBatch {
                owner_epoch: OwnerEpoch::LOCAL,
                command: None,
                facts,
                turn: None,
            },
        )
        .await
        .unwrap();
    store
}

fn idle(screen: &str) -> bool {
    screen.contains("FIFTH-ANSWER") && !screen.contains("esc to interrupt")
}

#[tokio::test(flavor = "multi_thread")]
async fn resuming_a_compacted_session_shows_the_whole_history() {
    let store = seeded().await;
    let model = GatedModel::scripted(vec![text_response(&["next reply"])]);
    let requests = model.clone();
    let setup = Setup {
        width: 100,
        height: 40,
        store,
        startup: rust_cli_tui::Startup::Session(SESSION.to_owned()),
        ..Setup::default()
    };
    with_ui(model, setup, |mut ui| async move {
        let screen = ui.wait("the resumed session", idle).await;
        for text in [
            "first question",
            "FIRST-ANSWER",
            "third question",
            "FIFTH-ANSWER",
        ] {
            assert!(screen.contains(text), "{text} is missing:\n{screen}");
        }
        // 每个压缩点一行；保留的回复只出现一次（在最后一个压缩点之后），摘要不显示。
        assert_eq!(screen.matches(COMPACTED).count(), 2, "{screen}");
        assert_eq!(screen.matches("FOURTH-ANSWER").count(), 1, "{screen}");
        assert!(!screen.contains("SUMMARY-TEXT"), "{screen}");
        let first = screen.find("FIRST-ANSWER").unwrap();
        let marks: Vec<usize> = screen.match_indices(COMPACTED).map(|(at, _)| at).collect();
        assert!(first < marks[0] && marks[1] < screen.find("FOURTH-ANSWER").unwrap());

        // ctrl+o：详细视图里同样有压缩前的内容。
        ui.press(KeyCode::Char('o'), KeyModifiers::CONTROL);
        ui.wait("the detailed transcript", |s| {
            s.contains("Showing detailed transcript")
        })
        .await;
        ui.press(KeyCode::Char('g'), KeyModifiers::NONE);
        ui.wait("the top of the detailed transcript", |s| {
            s.contains("first question")
        })
        .await;
        ui.press(KeyCode::Esc, KeyModifiers::NONE);

        // 下一个请求只从最后一个压缩点开始：摘要 2 与保留的回复在，压缩前的输入不在。
        ui.wait("the input box", |s| {
            !s.contains("Showing detailed transcript")
        })
        .await;
        ui.submit("next question");
        ui.wait("the reply", |s| s.contains("next reply")).await;
        let sent = requests.requests();
        let text = request_text(sent.last().unwrap());
        assert!(
            text.contains("SUMMARY-TEXT-2") && text.contains("FOURTH-ANSWER"),
            "{text}"
        );
        for gone in [
            "first question",
            "FIRST-ANSWER",
            "third question",
            "SUMMARY-TEXT-1",
        ] {
            assert!(!text.contains(gone), "{gone} was sent: {text}");
        }
    })
    .await;
}
