use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, ReadFile, WritePrecondition};
use rust_cli_kernel_api::env_files::PutFile;
use rust_cli_kernel_api::ids::MessageId;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent, RewindScope, SessionHandle};
use rust_cli_kernel_api::store::{ArtifactPurpose, Fact};
use rust_cli_kernel_api::testing::{
    MemoryEnvironment, ScriptedModel, text_response, tool_use_response,
};
use rust_cli_kernel_api::tool::{Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::test_support::{Built, Setup, envelope, finished, submit, until};

/// 写文件并按 Node 格式记 checkpoint 的工具（tools 的 Write 的最小替身）。
pub(crate) struct Writing;

#[async_trait]
impl Tool for Writing {
    fn name(&self) -> &str {
        "Write"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        "write".into()
    }
    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let path = input["file_path"].as_str().unwrap_or_default().to_owned();
        let after = input["content"].as_str().unwrap_or_default().to_owned();
        let read = ReadFile {
            path: EnvPath::new(&path),
        };
        let before = context
            .machine
            .files()
            .read_file(&context.workspace, read)
            .await
            .ok()
            .map(|file| String::from_utf8(file.bytes).unwrap_or_default());
        let put = PutFile {
            path: EnvPath::new(&path),
            bytes: after.clone().into_bytes(),
            precondition: WritePrecondition::Any,
            create_parents: true,
        };
        context
            .machine
            .files()
            .put_file(&context.workspace, context.operation.clone(), put)
            .await?;
        let artifact = json!({"files": [{"path": path, "existedBefore": before.is_some(),
            "beforeContent": before, "afterContent": after, "structuredPatch": []}]});
        let reference = context
            .artifacts
            .put(
                &context.session,
                ArtifactPurpose::Checkpoint,
                "application/json".into(),
                artifact.to_string().into_bytes(),
            )
            .await
            .map_err(|error| ToolError::Failed {
                message: error.to_string(),
            })?;
        Ok(ToolOutput {
            content: vec![rust_cli_kernel_api::message::ToolResultContent::Text {
                text: "written".into(),
            }],
            is_error: false,
            display: json!({"checkpoint": {"artifact": reference, "path": path,
                "additions": 1, "deletions": 0}}),
        })
    }
}

/// 一轮“写 a.txt”的会话；返回会话、执行环境与第一条 user 消息。
pub(crate) async fn wrote() -> (Built, Arc<MemoryEnvironment>, MessageId) {
    let model = Arc::new(ScriptedModel::new(vec![
        tool_use_response(
            "toolu_1",
            "Write",
            &json!({"file_path": "/work/a.txt", "content": "new\n"}),
        ),
        text_response(&["done"]),
    ]));
    let env = crate::test_support::env();
    env.set_file("/work/a.txt", "old\n");
    let mut setup = Setup::new(model);
    setup.set_env(env.clone());
    setup.tools = vec![Arc::new(Writing)];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "write it")).await;
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(
        |e| matches!(e, KernelEvent::FileChanges { files, rewindable: true, .. }
        if files[0].path == "/work/a.txt")
    ));
    let prompt = built.session.view().await.unwrap().messages[0].id().clone();
    (built, env, prompt)
}

pub(crate) fn file(env: &MemoryEnvironment, path: &str) -> Option<String> {
    let workspace = crate::test_support::meta().workspace;
    futures_util::FutureExt::now_or_never(env_read(env, workspace, path)).flatten()
}

async fn env_read(
    env: &MemoryEnvironment,
    workspace: rust_cli_kernel_api::env::WorkspaceRef,
    path: &str,
) -> Option<String> {
    use rust_cli_kernel_api::machine::Files;
    let read = ReadFile {
        path: EnvPath::new(path),
    };
    env.read_file(&workspace, read)
        .await
        .ok()
        .map(|f| String::from_utf8(f.bytes).unwrap_or_default())
}

/// checkpoint 与工具结果一起落库：`messageId` 是回合的 user 消息，`toolMessageId` 是 assistant。
#[tokio::test]
async fn checkpoints_are_committed_with_their_tool_results() {
    let (built, _env, prompt) = wrote().await;
    let facts = built.store.facts(&crate::test_support::meta().id);
    let index = facts
        .iter()
        .position(|f| matches!(f, Fact::CheckpointCreated { .. }))
        .unwrap();
    let Fact::CheckpointCreated { checkpoint } = &facts[index] else {
        unreachable!()
    };
    assert_eq!(checkpoint.prompt, prompt);
    assert_eq!(checkpoint.tool_use.as_str(), "toolu_1");
    // 同一批：前一条事实就是工具结果。
    assert!(
        matches!(&facts[index - 1], Fact::MessageAppended { message: Message::User(user) }
        if matches!(&user.origin, rust_cli_kernel_api::message::UserOrigin::ToolResult { assistant }
            if *assistant == checkpoint.assistant))
    );
    let options = built.session.rewind_options().await.unwrap();
    assert_eq!(options.len(), 1);
    assert!(options[0].code);
    assert_eq!(options[0].files[0].path, "/work/a.txt");
    assert_eq!(
        (options[0].files[0].additions, options[0].files[0].deletions),
        (1, 1)
    );
}

/// both：先恢复代码（核对 hash），再截对话；之后 checkpoint 不再可用。
#[tokio::test]
async fn rewinding_code_and_conversation_restores_the_file_and_cuts_history() {
    let (built, env, prompt) = wrote().await;
    assert_eq!(file(&env, "/work/a.txt").as_deref(), Some("new\n"));
    let mut events = built.session.subscribe(None);
    let command = Command::Rewind {
        target: prompt.clone(),
        scope: RewindScope::Both,
    };
    let ack = built.session.send(envelope("r1", command)).await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}")
    };
    assert_eq!(
        result["restored"],
        json!([{"path": "/work/a.txt", "action": "restore"}])
    );
    assert_eq!(result["removed"], 4);
    assert_eq!(file(&env, "/work/a.txt").as_deref(), Some("old\n"));
    until(&mut events, |e| {
        matches!(e, KernelEvent::HistoryRewritten { generation: 1 })
    })
    .await;
    let view = built.session.view().await.unwrap();
    assert!(view.messages.is_empty());
    let facts = built.store.facts(&crate::test_support::meta().id);
    let tail: Vec<&Fact> = facts.iter().rev().take(2).collect();
    assert!(matches!(tail[0], Fact::RevertSet { cursor } if cursor.target == prompt));
    assert!(matches!(tail[1], Fact::CheckpointsRestored { checkpoints } if checkpoints.len() == 1));
    assert!(built.session.rewind_options().await.unwrap().is_empty());
}

/// 文件在工具写入之后又被改过：整体拒绝，文件与对话都不动。
#[tokio::test]
async fn a_conflicting_file_rejects_the_whole_rewind() {
    let (built, env, prompt) = wrote().await;
    env.set_file("/work/a.txt", "edited by hand\n");
    let command = Command::Rewind {
        target: prompt,
        scope: RewindScope::Both,
    };
    let ack = built.session.send(envelope("r1", command)).await;
    assert!(
        matches!(&ack, Ack::Rejected { reason }
        if reason == "Cannot restore code: /work/a.txt was changed after ZCode edited it"),
        "{ack:?}"
    );
    assert_eq!(
        file(&env, "/work/a.txt").as_deref(),
        Some("edited by hand\n")
    );
    assert_eq!(built.session.view().await.unwrap().messages.len(), 4);
    // 代码回退只动文件；对话回退只动历史。
    let command = Command::Rewind {
        target: built.session.view().await.unwrap().messages[0].id().clone(),
        scope: RewindScope::Conversation,
    };
    let ack = built.session.send(envelope("r2", command)).await;
    assert!(matches!(ack, Ack::Accepted { .. }));
    assert_eq!(
        file(&env, "/work/a.txt").as_deref(),
        Some("edited by hand\n")
    );
    assert!(built.session.view().await.unwrap().messages.is_empty());
}
