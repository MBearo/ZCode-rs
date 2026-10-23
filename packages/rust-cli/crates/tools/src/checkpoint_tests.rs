use serde_json::json;

use super::structured_patch;

#[test]
fn a_changed_middle_line_is_one_hunk_with_context() {
    let before = "1\n2\n3\n4\n5\n6\n7\n8\n9\n";
    let after = "1\n2\n3\n4\nfive\n6\n7\n8\n9\n";
    let (hunks, additions, deletions) = structured_patch(before, after);
    assert_eq!((additions, deletions), (1, 1));
    assert_eq!(
        hunks,
        [
            json!({"oldStart": 2, "oldLines": 7, "newStart": 2, "newLines": 7,
                "lines": [" 2", " 3", " 4", "-5", "+five", " 6", " 7", " 8"]})
        ]
    );
}

#[test]
fn a_created_file_adds_every_line_and_identical_text_has_no_hunk() {
    let (hunks, additions, deletions) = structured_patch("", "a\nb\n");
    assert_eq!((additions, deletions), (2, 0));
    assert_eq!(
        hunks,
        [
            json!({"oldStart": 0, "oldLines": 0, "newStart": 1, "newLines": 2,
                "lines": ["+a", "+b"]})
        ]
    );
    assert_eq!(structured_patch("same\n", "same\n"), (Vec::new(), 0, 0));
}

mod tools {
    use std::sync::Arc;

    use rust_cli_kernel_api::env::{
        EnvFacts, EnvPath, Environment, OperationContext, Origin, ReadFile, WorkspaceRef,
    };
    use rust_cli_kernel_api::ids::{
        EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId, ToolUseId,
    };
    use rust_cli_kernel_api::permission::PermissionMode;
    use rust_cli_kernel_api::store::{ArtifactRef, ArtifactStore};
    use rust_cli_kernel_api::testing::{MemoryArtifacts, MemoryEnvironment, RecordingSink};
    use rust_cli_kernel_api::tool::{Tool, ToolUseContext};
    use serde_json::{Value, json};
    use tokio::sync::mpsc;
    use tokio_util::sync::CancellationToken;

    use crate::{FileTools, ReadFileState, WriteTool};

    fn facts() -> EnvFacts {
        EnvFacts {
            cwd: EnvPath::new("/work"),
            platform: "darwin".into(),
            os_version: "x".into(),
            shell: "zsh".into(),
            is_git_repo: false,
            additional_directories: Vec::new(),
            home: None,
        }
    }

    fn context(
        env: &Arc<MemoryEnvironment>,
        artifacts: &Arc<MemoryArtifacts>,
        call: &str,
    ) -> ToolUseContext {
        let session = SessionId::new("sess_1");
        ToolUseContext {
            session: session.clone(),
            run: RunId::new("run_1"),
            tool_use: ToolUseId::new(call),
            agent: None,
            workspace: WorkspaceRef {
                environment: EnvironmentId::new("local"),
                root: EnvPath::new("/work"),
            },
            mode: PermissionMode::AcceptEdits,
            cancel: CancellationToken::new(),
            env: env.clone(),
            artifacts: artifacts.clone(),
            interactions: Arc::new(RecordingSink::default()),
            operation: OperationContext {
                environment: EnvironmentId::new("local"),
                generation: Generation::new("g"),
                owner_epoch: OwnerEpoch::LOCAL,
                operation: OperationId::new(format!("op_{call}")),
                origin: Origin::Agent {
                    session,
                    run: RunId::new("run_1"),
                },
            },
        }
    }

    async fn artifact(artifacts: &MemoryArtifacts, display: &Value) -> Value {
        let reference: ArtifactRef =
            serde_json::from_value(display["checkpoint"]["artifact"].clone()).unwrap();
        serde_json::from_slice(&artifacts.get(&reference).await.unwrap()).unwrap()
    }

    /// Write 记下写入前后的磁盘内容（Node `workspace_file_before_change`），引用放进 display。
    #[tokio::test]
    async fn writes_record_node_checkpoints_with_before_and_after_content() {
        let env = Arc::new(MemoryEnvironment::new(facts()));
        let artifacts = Arc::new(MemoryArtifacts::default());
        let gate = Arc::new(rust_cli_policy::PolicyGate::default());
        let tool = WriteTool::new(FileTools::new(Arc::new(ReadFileState::default()), gate));
        let input = json!({"file_path": "/work/a.txt", "content": "one\n"});
        let (progress, _) = mpsc::channel(4);
        let created = tool
            .call(
                input,
                context(&env, &artifacts, "toolu_1"),
                progress.clone(),
            )
            .await
            .unwrap();
        let checkpoint = &created.display["checkpoint"];
        assert_eq!(checkpoint["path"], "/work/a.txt");
        assert_eq!(
            (
                checkpoint["additions"].clone(),
                checkpoint["deletions"].clone()
            ),
            (json!(1), json!(0))
        );
        let first = artifact(&artifacts, &created.display).await;
        let keys: Vec<&String> = first.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "createdAt",
                "files",
                "kind",
                "toolCallId",
                "toolName",
                "version"
            ]
        );
        assert_eq!(first["kind"], "workspace_file_before_change");
        assert_eq!(first["toolCallId"], "toolu_1");
        assert_eq!(first["toolName"], "Write");
        assert_eq!(
            first["files"][0],
            json!({"afterContent": "one\n", "afterContentLength": 4, "beforeContent": null,
                   "existedBefore": false, "path": "/work/a.txt",
                   "structuredPatch": [{"oldStart": 0, "oldLines": 0, "newStart": 1, "newLines": 1, "lines": ["+one"]}]})
        );
        let input = json!({"file_path": "/work/a.txt", "content": "two\n"});
        let updated = tool
            .call(input, context(&env, &artifacts, "toolu_2"), progress)
            .await
            .unwrap();
        let second = artifact(&artifacts, &updated.display).await;
        assert_eq!(second["files"][0]["beforeContent"], "one\n");
        assert_eq!(second["files"][0]["afterContent"], "two\n");
        assert_eq!(second["files"][0]["existedBefore"], true);
        let disk = env
            .read_file(
                &context(&env, &artifacts, "x").workspace,
                ReadFile {
                    path: EnvPath::new("/work/a.txt"),
                },
            )
            .await
            .unwrap();
        assert_eq!(disk.bytes, b"two\n");
        assert_eq!(artifacts.count(), 2);
    }
}
