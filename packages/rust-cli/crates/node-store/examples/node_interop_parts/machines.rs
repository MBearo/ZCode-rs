//! 互操作的第二十四段：远端机器上的工具调用（docs/specs/rust-cli/remote-exec.md §20.9，R3）。
//!
//! `machines-write <storage-dir>`：会话 actor 的写法——机器清单提醒（`remote_machines`）；一条回复里 Bash 在 dev-a 上
//! 跑（输入带 `machine`）、Write 在 dev-a 上改文件（输入带 `machine`）、Write 在本机改文件；工具结果与两个 checkpoint
//! 同一批（本机的写成 Node 的 `runtime/workspace_checkpoint`，远端的写成 `rust-cli/remote_checkpoint`）；再一条文字回复。
//! 输出会话 id、回复、提醒与两个 checkpoint 的 id。
use rust_cli_kernel_api::store::{ArtifactPurpose, ArtifactStore, WorkspaceCheckpoint};

use super::*;

const REMOTE_FILE: &str = "/home/dev/proj/out.txt";
const LOCAL_FILE: &str = "/w/interop/local.txt";

/// Node `workspace_file_before_change` 的形状（远端的另加 `machine`）。
async fn artifact(
    store: &NodeStore,
    id: &SessionId,
    (call, path, machine): (&str, &str, Option<&str>),
) -> Result<rust_cli_kernel_api::store::ArtifactRef> {
    let mut artifact = json!({
        "createdAt": "2026-10-24T00:00:00.000Z",
        "files": [{"afterContent": "new\n", "afterContentLength": 4, "beforeContent": null,
            "existedBefore": false, "path": path,
            "structuredPatch": [{"oldStart": 0, "oldLines": 0, "newStart": 1, "newLines": 1,
                "lines": ["+new"]}]}],
        "kind": "workspace_file_before_change",
        "toolCallId": call,
        "toolName": "Write",
        "version": 1,
    });
    if let Some(machine) = machine {
        artifact["machine"] = json!(machine);
    }
    Ok(store
        .put(
            id,
            ArtifactPurpose::Checkpoint,
            "application/json".into(),
            serde_json::to_vec_pretty(&artifact)?,
        )
        .await?)
}

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let id = create(&store).await?;
    let mut current = None;
    let (ask, notice, reply_id, answer) = (message_id(), message_id(), message_id(), message_id());
    let machines = Fact::MessageAppended {
        message: Message::Attachment(AttachmentMessage {
            id: notice.clone(),
            source: "remote_machines".into(),
            text: "<system-reminder>\nRemote machines connected to this session.\n- dev-a: Linux x86_64, shell /bin/bash, working directory /home/dev/proj\n</system-reminder>".into(),
            data: json!({"machines": [{"name": "dev-a", "os": "linux", "arch": "x86_64",
                "shell": "/bin/bash", "workingDirectory": "/home/dev/proj"}]}),
        }),
    };
    let bash = json!({"command": "uname -m", "description": "arch", "machine": "dev-a"});
    let remote_write = json!({"file_path": REMOTE_FILE, "content": "new\n", "machine": "dev-a"});
    let local_write = json!({"file_path": LOCAL_FILE, "content": "new\n"});
    store
        .commit(
            &id,
            anchored(&mut current, vec![user(&ask, "Check dev-a"), machines]),
        )
        .await?;
    let calls = reply(
        &reply_id,
        vec![
            call("toolu_bash", "Bash", &bash),
            call("toolu_remote", "Write", &remote_write),
            call("toolu_local", "Write", &local_write),
        ],
        StopReason::ToolUse,
    );
    store
        .commit(&id, anchored(&mut current, vec![calls]))
        .await?;
    let remote_snapshot =
        artifact(&store, &id, ("toolu_remote", REMOTE_FILE, Some("dev-a"))).await?;
    let local_snapshot = artifact(&store, &id, ("toolu_local", LOCAL_FILE, None)).await?;
    let checkpoint =
        |name: &str, call: &str, snapshot, machine: Option<&str>| WorkspaceCheckpoint {
            id: name.into(),
            prompt: ask.clone(),
            assistant: reply_id.clone(),
            tool_use: ToolUseId::new(call),
            snapshot,
            machine: machine.map(str::to_owned),
        };
    let facts = vec![
        result(&reply_id, "toolu_bash", vec![said("x86_64")], false),
        result(
            &reply_id,
            "toolu_remote",
            vec![said("File created successfully")],
            false,
        ),
        result(
            &reply_id,
            "toolu_local",
            vec![said("File created successfully")],
            false,
        ),
        Fact::CheckpointCreated {
            checkpoint: checkpoint(
                "checkpoint_remote",
                "toolu_remote",
                remote_snapshot,
                Some("dev-a"),
            ),
        },
        Fact::CheckpointCreated {
            checkpoint: checkpoint("checkpoint_local", "toolu_local", local_snapshot, None),
        },
    ];
    store.commit(&id, anchored(&mut current, facts)).await?;
    let done = reply(&answer, vec![text("dev-a is x86_64.")], StopReason::EndTurn);
    store
        .commit(&id, anchored(&mut current, vec![done]))
        .await?;
    print(&json!({
        "session": id.as_str(),
        "reply": reply_id.as_str(),
        "notice": notice.as_str(),
        "remoteFile": REMOTE_FILE,
    }))
}
