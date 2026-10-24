//! 按机器分开的会话状态（remote-exec.md §20.7）：远端机器上的后台通知 ack 到那台机器、会话关闭也通知它；跨机器的
//! `/rewind` 全部 prepare 再 apply，prepare 失败整体拒绝，apply 中途失败逐台报告。

use std::sync::Arc;

use async_trait::async_trait;
use rust_cli_kernel_api::env::{EnvPath, ReadFile, WritePrecondition};
use rust_cli_kernel_api::env_files::PutFile;
use rust_cli_kernel_api::env_registry::EnvironmentRegistry;
use rust_cli_kernel_api::session_api::{Ack, Command, KernelEvent, RewindScope, SessionHandle};
use rust_cli_kernel_api::store::{ArtifactPurpose, Fact};
use rust_cli_kernel_api::testing::{
    MemoryEnvironment, NoticeEnvironment, ScriptedModel, TestMachines, shell_notice, text_response,
};
use rust_cli_kernel_api::tool::{Tool, ToolError, ToolOutput, ToolProgress, ToolUseContext};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::rewind::tests::{Writing, file};
use crate::test_support::{Setup, envelope, finished, submit, until};

/// 在 dev-a 上写文件并记 checkpoint（带 `machine`）的工具：tools 的 Write 路由到远端之后的最小替身。
struct WritingOnDevA(Arc<TestMachines>);

#[async_trait]
impl Tool for WritingOnDevA {
    fn name(&self) -> &str {
        "RemoteWrite"
    }
    fn input_schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn description(&self, _context: &rust_cli_kernel_api::tool::PromptContext) -> String {
        "write on dev-a".into()
    }
    async fn call(
        &self,
        input: Value,
        context: ToolUseContext,
        _progress: mpsc::Sender<ToolProgress>,
    ) -> Result<ToolOutput, ToolError> {
        let (machine, workspace) = self.0.remote("dev-a").unwrap();
        let path = input["file_path"].as_str().unwrap_or_default().to_owned();
        let after = input["content"].as_str().unwrap_or_default().to_owned();
        let read = ReadFile {
            path: EnvPath::new(&path),
        };
        let before = machine
            .files()
            .read_file(&workspace, read)
            .await
            .ok()
            .map(|file| String::from_utf8(file.bytes).unwrap_or_default());
        let put = PutFile {
            path: EnvPath::new(&path),
            bytes: after.clone().into_bytes(),
            precondition: WritePrecondition::Any,
            create_parents: true,
        };
        machine
            .files()
            .put_file(&workspace, context.operation.clone(), put)
            .await?;
        let artifact = json!({"files": [{"path": path, "existedBefore": before.is_some(),
            "beforeContent": before, "afterContent": after, "structuredPatch": []}],
            "machine": "dev-a"});
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
                "additions": 1, "deletions": 0, "machine": "dev-a"}}),
        })
    }
}

struct Two {
    built: crate::test_support::Built,
    local: Arc<MemoryEnvironment>,
    remote: Arc<MemoryEnvironment>,
    registry: Arc<TestMachines>,
}

/// 一轮里本机写 /work/a.txt、dev-a 写 /home/dev/proj/b.txt。
async fn wrote_on_two_machines() -> (Two, rust_cli_kernel_api::ids::MessageId) {
    let model = Arc::new(ScriptedModel::new(vec![
        rust_cli_kernel_api::testing::tool_use_response(
            "toolu_1",
            "Write",
            &json!({"file_path": "/work/a.txt", "content": "new a\n"}),
        ),
        rust_cli_kernel_api::testing::tool_use_response(
            "toolu_2",
            "RemoteWrite",
            &json!({"file_path": "/home/dev/proj/b.txt", "content": "new b\n"}),
        ),
        text_response(&["done"]),
    ]));
    let local = crate::test_support::env();
    local.set_file("/work/a.txt", "old a\n");
    let remote = crate::test_support::env();
    remote.set_file("/home/dev/proj/b.txt", "old b\n");
    let registry = TestMachines::new(local.clone());
    registry.connect("dev-a", remote.clone(), "/home/dev/proj");
    let mut setup = Setup::new(model);
    setup.set_env(local.clone());
    setup.machines = Some(registry.clone());
    setup.tools = vec![Arc::new(Writing), Arc::new(WritingOnDevA(registry.clone()))];
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    built.session.send(submit("c1", "write both")).await;
    until(&mut events, finished).await;
    let prompt = built.session.view().await.unwrap().messages[0].id().clone();
    (
        Two {
            built,
            local,
            remote,
            registry,
        },
        prompt,
    )
}

/// token 在回复之后释放（后台任务）：等到释放了 `count` 个（2 s 内）。
async fn released(env: &MemoryEnvironment, count: usize) {
    for _ in 0..200 {
        if env.rewinds_finished().len() >= count {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(env.rewinds_finished().len(), count);
}

fn rewind(target: &rust_cli_kernel_api::ids::MessageId) -> Command {
    Command::Rewind {
        target: target.clone(),
        scope: RewindScope::Both,
    }
}

#[tokio::test]
async fn a_remote_checkpoint_carries_its_machine_and_both_machines_are_restored() {
    let (two, prompt) = wrote_on_two_machines().await;
    let facts = two.built.store.facts(&crate::test_support::meta().id);
    let machines: Vec<Option<String>> = facts
        .iter()
        .filter_map(|f| match f {
            Fact::CheckpointCreated { checkpoint } => Some(checkpoint.machine.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(machines, [None, Some("dev-a".to_owned())]);
    let ack = two
        .built
        .session
        .send(envelope("r1", rewind(&prompt)))
        .await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}")
    };
    assert_eq!(
        result["restored"],
        json!([
            {"path": "/work/a.txt", "action": "restore"},
            {"path": "/home/dev/proj/b.txt", "action": "restore", "machine": "dev-a"}
        ])
    );
    assert_eq!(file(&two.local, "/work/a.txt").as_deref(), Some("old a\n"));
    assert_eq!(
        file(&two.remote, "/home/dev/proj/b.txt").as_deref(),
        Some("old b\n")
    );
    // 两台机器的 token 都释放了。
    released(&two.local, 1).await;
    released(&two.remote, 1).await;
}

#[tokio::test]
async fn a_conflict_on_one_machine_rejects_the_whole_rewind() {
    let (two, prompt) = wrote_on_two_machines().await;
    two.remote
        .set_file("/home/dev/proj/b.txt", "edited by hand\n");
    let ack = two
        .built
        .session
        .send(envelope("r1", rewind(&prompt)))
        .await;
    assert!(
        matches!(&ack, Ack::Rejected { reason }
            if reason == "dev-a: Cannot restore code: /home/dev/proj/b.txt was changed after ZCode edited it"),
        "{ack:?}"
    );
    // 本机的文件也不动；本机已经 prepare 的 token 释放了。
    assert_eq!(file(&two.local, "/work/a.txt").as_deref(), Some("new a\n"));
    released(&two.local, 1).await;
    assert_eq!(two.built.session.view().await.unwrap().messages.len(), 6);
}

#[tokio::test]
async fn a_disconnected_machine_rejects_the_rewind() {
    let (two, prompt) = wrote_on_two_machines().await;
    two.registry.disconnect("dev-a");
    let ack = two
        .built
        .session
        .send(envelope("r1", rewind(&prompt)))
        .await;
    assert!(
        matches!(&ack, Ack::Rejected { reason }
            if reason == "Cannot restore code: Machine \"dev-a\" is not connected."),
        "{ack:?}"
    );
    assert_eq!(file(&two.local, "/work/a.txt").as_deref(), Some("new a\n"));
}

#[tokio::test]
async fn an_apply_failure_reports_each_machine() {
    let (two, prompt) = wrote_on_two_machines().await;
    two.remote.fail_next_rewind_apply();
    let ack = two
        .built
        .session
        .send(envelope("r1", rewind(&prompt)))
        .await;
    assert!(
        matches!(&ack, Ack::Rejected { reason } if reason == "local: restored\ndev-a: disk full"),
        "{ack:?}"
    );
    // 本机已经改回，记下了；dev-a 没改；对话不动。
    assert_eq!(file(&two.local, "/work/a.txt").as_deref(), Some("old a\n"));
    assert_eq!(
        file(&two.remote, "/home/dev/proj/b.txt").as_deref(),
        Some("new b\n")
    );
    let facts = two.built.store.facts(&crate::test_support::meta().id);
    assert!(
        matches!(facts.last(), Some(Fact::CheckpointsRestored { checkpoints }) if checkpoints.len() == 1)
    );
    assert_eq!(two.built.session.view().await.unwrap().messages.len(), 6);
    // 只剩 dev-a 那一个可以回退。
    let options = two.built.session.rewind_options().await.unwrap();
    assert_eq!(options[0].files.len(), 1);
    assert_eq!(options[0].files[0].path, "/home/dev/proj/b.txt");
}

#[tokio::test]
async fn a_notice_from_a_remote_machine_is_acknowledged_there() {
    let local = Arc::new(NoticeEnvironment::default());
    let remote = Arc::new(NoticeEnvironment::default());
    let registry = TestMachines::new(local.clone());
    let model = Arc::new(ScriptedModel::new(vec![text_response(&["seen"])]));
    let mut setup = Setup::new(model);
    setup.set_env(local.clone());
    setup.machines = Some(registry.clone());
    let built = setup.build().await;
    let mut events = built.session.subscribe(Some(0));
    // 会话打开之后才连上的机器。
    registry.connect("dev-a", remote.clone(), "/home/dev/proj");
    remote.finish(shell_notice(1, "b0000000r"));
    let seen = until(&mut events, finished).await;
    assert!(seen.iter().any(|event| matches!(
        event,
        KernelEvent::BackgroundWork { work, .. } if work == "b0000000r"
    )));
    assert_eq!(remote.acked(), [1]);
    assert!(local.acked().is_empty());
    // 关闭会话：远端机器也收到 close_session。
    built
        .session
        .close(rust_cli_kernel_api::session_api::EndReason::Other)
        .await;
    assert_eq!(remote.closed().len(), 1);
    assert_eq!(local.closed().len(), 1);
}
