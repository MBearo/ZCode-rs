//! 互操作的第四段会话：带压缩边界（docs/specs/rust-cli/compact.md §6）。
//!
//! 两轮对话后手动压缩：保留尾段是最后一轮的工具调用与结果（尾段以工具结果结尾，Node 记录里映射成
//! 它的 assistant 行），边界、摘要与文件附件一个事务提交；压缩之后再问答一轮。
use rust_cli_kernel_api::message::{
    CompactBoundary, CompactTrigger, PreservedSegment, SystemMessage,
};

use super::*;

pub(super) async fn compacted(store: &NodeStore) -> Result<SessionId> {
    let id = create(store).await?;
    let read = json!({"file_path": "/w/interop/a.rs"});
    let (u1, a1, u2, a2, r2) = (
        message_id(),
        message_id(),
        message_id(),
        message_id(),
        message_id(),
    );
    store
        .commit(&id, batch(Some("cmd_c1"), vec![user(&u1, "What is a.rs?")]))
        .await?;
    let answer = reply(&a1, vec![text("Let me look.")], StopReason::EndTurn);
    store.commit(&id, batch(None, vec![answer])).await?;
    store
        .commit(&id, batch(Some("cmd_c2"), vec![user(&u2, "Read it now")]))
        .await?;
    let calls = reply(
        &a2,
        vec![call("toolu_c1", "Read", &read)],
        StopReason::ToolUse,
    );
    store
        .commit(
            &id,
            batch(None, vec![intent("op_c1", "Read", &read), calls]),
        )
        .await?;
    let result = Fact::MessageAppended {
        message: Message::User(UserMessage {
            id: r2.clone(),
            content: vec![ContentBlock::ToolResult {
                tool_use_id: ToolUseId::new("toolu_c1"),
                content: vec![said("     1\tfn main() {}")],
                is_error: false,
            }],
            origin: UserOrigin::ToolResult {
                assistant: a2.clone(),
            },
        }),
    };
    store.commit(&id, batch(None, vec![result])).await?;

    let (boundary, summary) = (message_id(), message_id());
    let facts = vec![
        Fact::MessageAppended {
            message: Message::System(SystemMessage::CompactBoundary(CompactBoundary {
                id: boundary,
                trigger: CompactTrigger::Manual,
                pre_tokens: 1200,
                summary: summary.clone(),
                preserved: Some(PreservedSegment {
                    head: a2.clone(),
                    anchor: summary.clone(),
                    tail: r2,
                }),
            })),
        },
        Fact::MessageAppended {
            message: Message::User(UserMessage {
                id: summary,
                content: vec![text(
                    "This session is being continued from a previous conversation that ran out of context. The summary below covers the earlier portion of the conversation.\n\nSummary:\nThe user asked about a.rs.",
                )],
                origin: UserOrigin::CompactSummary,
            }),
        },
        attachment(
            "file",
            "<system-reminder>\nCalled the Read tool with the following input: {\"file_path\":\"/w/interop/a.rs\"}\n\nResult of calling the Read tool:\n     1\tfn main() {}\n</system-reminder>",
            json!({"filename": "/w/interop/a.rs"}),
        ),
    ];
    store.commit(&id, batch(None, facts)).await?;
    let (u3, a3) = (message_id(), message_id());
    store
        .commit(&id, batch(Some("cmd_c3"), vec![user(&u3, "Thanks")]))
        .await?;
    let answer = reply(&a3, vec![text("You are welcome.")], StopReason::EndTurn);
    store.commit(&id, batch(None, vec![answer])).await?;
    Ok(id)
}
