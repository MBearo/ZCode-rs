//! 互操作的第七段：Node 的 `/fork`（docs/specs/rust-cli/session-lifecycle.md §6.5）。
//!
//! `fork <storage-dir> <session>`：从会话 checkpoint 清单里最后一个 checkpoint 分出新会话（`SessionStore::fork`，
//! 文件当作已恢复：清单里的那个文件记为 `restore`），输出新会话 id 与复制的消息数。
use rust_cli_kernel_api::env_rewind::{RestoreAction, RestoredFile};
use rust_cli_kernel_api::store::SessionFork;

use super::*;

pub(super) async fn fork(dir: &str, session: &str) -> Result<()> {
    let store = store(dir)?;
    let parent = SessionId::new(session);
    let loaded = store.load(&parent).await?;
    let checkpoint = loaded
        .checkpoints
        .last()
        .cloned()
        .context("the session has no checkpoint")?;
    let child = ids().session();
    let fork = SessionFork {
        parent,
        child: child.clone(),
        created_at_ms: now_ms(),
        through: checkpoint.prompt.clone(),
        checkpoint: Some(checkpoint),
        restored: vec![RestoredFile {
            path: EnvPath::new(super::checkpoint::CHECKPOINT_FILE),
            action: RestoreAction::Restore,
        }],
        selection: loaded.model.unwrap_or(ModelSelection {
            provider: "p".into(),
            model: "m".into(),
            reasoning: None,
            connection: None,
        }),
        mode: loaded.mode.unwrap_or(PermissionMode::Default),
        plan: loaded.plan,
    };
    let receipt = store.fork(fork).await?;
    print(&json!({"session": child.as_str(), "copied": receipt.copied}))
}
