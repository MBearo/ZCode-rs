//! 互操作的第十八段：续写占位（docs/specs/rust-cli/node-store.md §4.2.1，工作线 F77、F79）。
//!
//! `sentinel-write <storage-dir> <timing>`：一轮被中断的输入（没有回答），之后按写入时机：
//! - `continue`：占位与新输入同一批（启动时打开、`-p` 续写），新输入有回答；
//! - `open`：占位单独一批（会话里切过去就写，工作线 F79），之后不再写（切过去就退出）。
//!
//! 输出会话 id 与占位的消息 id。
use rust_cli_kernel_api::message::{NO_RESPONSE_REQUESTED, SYNTHETIC_MODEL};

use super::*;

pub(super) async fn write(dir: &str, timing: &str) -> Result<()> {
    let store = store(dir)?;
    let id = create(&store).await?;
    let mut current = None;
    let (first, placeholder, second, answer) =
        (message_id(), message_id(), message_id(), message_id());
    store
        .commit(
            &id,
            anchored(&mut current, vec![user(&first, "First question")]),
        )
        .await?;
    let resumed = Fact::MessageAppended {
        message: Message::Assistant(AssistantMessage {
            id: placeholder.clone(),
            content: vec![text(NO_RESPONSE_REQUESTED)],
            model: SYNTHETIC_MODEL.into(),
            stop_reason: Some(StopReason::StopSequence),
            usage: None,
        }),
    };
    match timing {
        "continue" => {
            store
                .commit(
                    &id,
                    anchored(
                        &mut current,
                        vec![resumed, user(&second, "Second question")],
                    ),
                )
                .await?;
            store
                .commit(
                    &id,
                    anchored(
                        &mut current,
                        vec![reply(
                            &answer,
                            vec![text("Second answer")],
                            StopReason::EndTurn,
                        )],
                    ),
                )
                .await?;
        }
        // 会话 actor 的写法：只有占位，锚在被中断的那一轮。
        "open" => {
            store
                .commit(&id, anchored(&mut current, vec![resumed]))
                .await?;
        }
        other => bail!("unknown timing {other:?} (expected continue or open)"),
    }
    print(&json!({"session": id.as_str(), "placeholder": placeholder.as_str()}))
}
