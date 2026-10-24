//! 互操作的第十九段：工具结果的位置键（docs/specs/rust-cli/node-store.md §4.6，工作线 F81）。
//!
//! `tool-order-write <storage-dir>`：一条回复里三个调用。两个 Read 的结果按交出顺序提交（second 先），各自后面跟一条
//! PostToolUse 的附加上下文（system 角色的 attachment）；Bash 没有结果时进程被杀，续写时补的中断结果、续写占位与新
//! 输入同一批（会话 actor 的写法）；新输入有回答。输出会话 id、回复与两段附加上下文的消息 id。
use rust_cli_kernel_api::message::{INTERRUPTED_CALL, NO_RESPONSE_REQUESTED, SYNTHETIC_MODEL};

use super::*;

fn context(id: &MessageId, body: &str) -> Fact {
    Fact::MessageAppended {
        message: Message::Attachment(AttachmentMessage {
            id: id.clone(),
            source: "hook_additional_context".into(),
            text: format!("<system-reminder>\n{body}\n</system-reminder>"),
            data: json!({"hookName": "PostToolUse:Read", "hookEvent": "PostToolUse", "content": [body]}),
        }),
    }
}

fn said(text: &str) -> ToolResultContent {
    ToolResultContent::Text { text: text.into() }
}

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let id = create(&store).await?;
    let mut current = None;
    let (ask, reply_id, second, first, again, answer) = (
        message_id(),
        message_id(),
        message_id(),
        message_id(),
        message_id(),
        message_id(),
    );
    let read_first = json!({"file_path": "/w/interop/first.txt"});
    let read_second = json!({"file_path": "/w/interop/second.txt"});
    let bash = json!({"command": "sleep 30"});
    let steps = vec![
        vec![user(&ask, "Read both files")],
        vec![reply(
            &reply_id,
            vec![
                text("Reading."),
                call("toolu_first", "Read", &read_first),
                call("toolu_second", "Read", &read_second),
                call("toolu_wait", "Bash", &bash),
            ],
            StopReason::ToolUse,
        )],
        vec![result(&reply_id, "toolu_second", vec![said("beta")], false)],
        vec![context(
            &second,
            "PostToolUse:Read hook additional context: ctx second",
        )],
        vec![result(&reply_id, "toolu_first", vec![said("alpha")], false)],
        vec![context(
            &first,
            "PostToolUse:Read hook additional context: ctx first",
        )],
        // 进程在 Bash 执行时被杀；续写时补的中断结果与占位排在新输入之前，同一批。
        vec![
            result(&reply_id, "toolu_wait", vec![said(INTERRUPTED_CALL)], true),
            Fact::MessageAppended {
                message: Message::Assistant(AssistantMessage {
                    id: message_id(),
                    content: vec![text(NO_RESPONSE_REQUESTED)],
                    model: SYNTHETIC_MODEL.into(),
                    stop_reason: Some(StopReason::StopSequence),
                    usage: None,
                }),
            },
            user(&again, "Again"),
        ],
        vec![reply(&answer, vec![text("Done.")], StopReason::EndTurn)],
    ];
    for facts in steps {
        store.commit(&id, anchored(&mut current, facts)).await?;
    }
    print(&json!({
        "session": id.as_str(),
        "reply": reply_id.as_str(),
        "contexts": [second.as_str(), first.as_str()],
        "again": again.as_str(),
    }))
}
