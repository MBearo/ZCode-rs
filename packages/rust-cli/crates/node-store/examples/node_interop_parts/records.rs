//! 互操作的第二十六段：本地命令的记录（修复线 L-A，docs/specs/rust-cli/tui-manage.md §11.3）。
//!
//! `records-write <storage-dir>`：一轮问答，之后一批本地命令的记录（固定说明、命令输入、输出、meta），再一轮问答。
//! 记录是会话 actor 写的 meta user 消息（来源 `local-command-*`）。输出会话 id 与四条记录的消息 id。
use rust_cli_kernel_api::user_command::{LocalCommandOutput, LocalCommandRecord};

use super::*;

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let id = create(&store).await?;
    let mut current = None;
    let (first, answer) = (message_id(), message_id());
    store
        .commit(
            &id,
            anchored(
                &mut current,
                vec![
                    user(&first, "First question"),
                    reply(&answer, vec![text("First answer")], StopReason::EndTurn),
                ],
            ),
        )
        .await?;
    let record = LocalCommandRecord {
        name: "rename".into(),
        args: "my-name".into(),
        output: LocalCommandOutput::Stdout("Session renamed to: my-name".into()),
        caveat: true,
        meta: vec![
            "<system-reminder>\nThe user named this session \"my-name\".\n</system-reminder>"
                .into(),
        ],
    };
    let mut ids = Vec::new();
    let facts = record
        .texts()
        .into_iter()
        .map(|(source, body)| {
            let id = message_id();
            ids.push(id.as_str().to_owned());
            Fact::MessageAppended {
                message: Message::User(UserMessage {
                    id,
                    content: vec![text(&body)],
                    origin: UserOrigin::Meta {
                        source: source.to_owned(),
                    },
                }),
            }
        })
        .collect();
    store.commit(&id, anchored(&mut current, facts)).await?;
    let (second, again) = (message_id(), message_id());
    store
        .commit(
            &id,
            anchored(
                &mut current,
                vec![
                    user(&second, "Second question"),
                    reply(&again, vec![text("Second answer")], StopReason::EndTurn),
                ],
            ),
        )
        .await?;
    print(&json!({"session": id.as_str(), "records": ids}))
}
