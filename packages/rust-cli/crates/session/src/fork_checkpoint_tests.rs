//! Node 的 `/fork`（session-lifecycle.md §6.5）：选 checkpoint、恢复文件（冲突整体拒绝）、`ForkFiles::Keep`、新会话的内容。
use std::sync::Arc;

use rust_cli_kernel_api::env_rewind::RestoreAction;
use rust_cli_kernel_api::ids::SessionId;
use rust_cli_kernel_api::message::Message;
use rust_cli_kernel_api::session_api::{Ack, Command, ForkFiles, SessionHandle};
use rust_cli_kernel_api::store::SessionStore;
use rust_cli_kernel_api::testing::ScriptedModel;
use serde_json::json;

use super::plan;
use crate::rewind::tests::{file, wrote};
use crate::rewind_plan::{Artifact, ArtifactFile};
use crate::test_support::{Setup, envelope};

fn fork(
    checkpoint: Option<&str>,
    files: ForkFiles,
) -> rust_cli_kernel_api::session_api::CommandEnvelope {
    envelope(
        "f1",
        Command::ForkCheckpoint {
            checkpoint: checkpoint.map(str::to_owned),
            files,
        },
    )
}

#[tokio::test]
async fn fork_latest_restores_the_file_and_writes_the_session() {
    let (built, env, prompt) = wrote().await;
    let checkpoints = built.session.checkpoints().await.unwrap();
    assert_eq!(checkpoints.len(), 1);
    let ack = built
        .session
        .send(fork(Some(&checkpoints[0].id), ForkFiles::Restore))
        .await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}")
    };
    assert_eq!(file(&env, "/work/a.txt").as_deref(), Some("old\n"));
    let child = SessionId::new(result["session"].as_str().unwrap());
    assert_eq!(
        result["response"],
        format!(
            "Forked session {child} from checkpoint {}: copied 1 messages and restored 1 file.",
            checkpoints[0].id
        )
    );
    assert_eq!(
        result["restored"],
        json!([{"path": "/work/a.txt", "action": "restore"}])
    );
    let loaded = built.store.load(&child).await.unwrap();
    assert_eq!(loaded.meta.parent, Some(crate::test_support::meta().id));
    assert_eq!(loaded.messages[0].id(), &prompt);
    assert!(matches!(&loaded.messages[1], Message::Attachment(a) if a.source == "fork"));
    // 原会话不变：checkpoint 仍在清单里（Node 同样不写回退记录）。
    assert_eq!(built.session.view().await.unwrap().messages.len(), 4);
    assert_eq!(built.session.checkpoints().await.unwrap().len(), 1);
}

#[tokio::test]
async fn a_conflict_rejects_the_fork_and_writes_nothing() {
    let (built, env, _prompt) = wrote().await;
    env.set_file("/work/a.txt", "edited by hand\n");
    let before = built
        .store
        .list(rust_cli_kernel_api::store::SessionFilter {
            workspace: crate::test_support::meta().workspace,
            limit: 0,
        })
        .await
        .unwrap()
        .len();
    let ack = built.session.send(fork(None, ForkFiles::Restore)).await;
    assert!(
        matches!(&ack, Ack::Rejected { reason }
            if reason == "Cannot restore code: /work/a.txt was changed after ZCode edited it"),
        "{ack:?}"
    );
    assert_eq!(
        file(&env, "/work/a.txt").as_deref(),
        Some("edited by hand\n")
    );
    let after = built
        .store
        .list(rust_cli_kernel_api::store::SessionFilter {
            workspace: crate::test_support::meta().workspace,
            limit: 0,
        })
        .await
        .unwrap()
        .len();
    assert_eq!(before, after);
}

#[tokio::test]
async fn keep_leaves_the_files_and_unknown_checkpoints_are_rejected() {
    let (built, env, prompt) = wrote().await;
    let ack = built
        .session
        .send(envelope(
            "f2",
            Command::ForkCheckpoint {
                checkpoint: Some("nope".to_owned()),
                files: ForkFiles::Restore,
            },
        ))
        .await;
    assert!(matches!(&ack, Ack::Rejected { reason } if reason == "Checkpoint not found: nope"));
    let ack = built.session.send(fork(None, ForkFiles::Keep)).await;
    let Ack::Accepted { result } = ack else {
        panic!("{ack:?}")
    };
    assert_eq!(file(&env, "/work/a.txt").as_deref(), Some("new\n"));
    let child = result["session"].as_str().unwrap();
    assert_eq!(
        result["response"],
        format!("Forked session {child} from message {prompt}: copied 1 messages.")
    );
    assert_eq!(result["restored"], json!([]));
    // 没有 checkpoint 的会话。
    let empty = Setup::new(Arc::new(ScriptedModel::new(Vec::new())))
        .build()
        .await;
    let ack = empty.session.send(fork(None, ForkFiles::Restore)).await;
    assert!(matches!(&ack, Ack::Rejected { reason }
        if reason == "No workspace checkpoint is available yet."));
}

fn written(path: &str, before: Option<&str>, after: &str) -> ArtifactFile {
    ArtifactFile {
        path: path.to_owned(),
        existed_before: before.is_some(),
        before_content: before.map(str::to_owned),
        after_content: Some(after.to_owned()),
    }
}

#[test]
fn the_plan_restores_the_selected_files_against_the_last_write() {
    use rust_cli_kernel_api::env_rewind::content_hash;
    let artifacts = [
        Artifact {
            files: vec![
                written("/w/a", Some("a0"), "a1"),
                written("/w/new", None, "n1"),
            ],
        },
        Artifact {
            files: vec![
                written("/w/a", Some("a1"), "a2"),
                written("/w/b", Some("b0"), "b1"),
            ],
        },
    ];
    let planned = plan(&artifacts).unwrap();
    // 只恢复所选 checkpoint 写过的文件（/w/b 不动，按 Node）；期望是最后一次写入后的内容。
    assert_eq!(planned.len(), 2);
    assert_eq!(planned[0].path.as_str(), "/w/a");
    assert_eq!(planned[0].expected, Some(content_hash(b"a2")));
    assert_eq!(planned[0].restore.as_deref(), Some(b"a0".as_slice()));
    assert_eq!(planned[1].path.as_str(), "/w/new");
    assert_eq!(planned[1].expected, Some(content_hash(b"n1")));
    assert_eq!(planned[1].restore, None);
    let _ = RestoreAction::Delete;
}
