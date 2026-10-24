//! 互操作的第七段：`/fork`（docs/specs/rust-cli/session-lifecycle.md §6.5，F75 起按用户输入分叉）。
//!
//! `fork <storage-dir> <session> <through>`：从会话分出新会话，复制到 `through`（脚本给出所选那一轮最后一条
//! assistant），文件不动（`SessionStore::fork` 分叉对话的记录），输出新会话 id 与复制的消息数。
use rust_cli_kernel_api::store::SessionFork;

use super::*;

pub(super) async fn fork(dir: &str, session: &str, through: &str) -> Result<()> {
    let store = store(dir)?;
    let parent = SessionId::new(session);
    let loaded = store.load(&parent).await?;
    let child = ids().session();
    let fork = SessionFork {
        parent,
        child: child.clone(),
        created_at_ms: now_ms(),
        through: MessageId::new(through),
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
