//! 互操作的第十三段：会话的改名与删除（修复线 F29，docs/specs/rust-cli/node-store.md §5.1）。
//!
//! - `manage-write <storage-dir>`：在 `/w/manage` 与 `/w/manage-other` 各写一个带首条输入的会话，输出会话 id；
//! - `rename <storage-dir> <session> <title>`、`delete <storage-dir> <session>`：`SessionStore::rename` / `delete`；
//! - `query <storage-dir> [<root>]`：`SessionStore::query_sessions`（按活动时间，上限 50），输出 `SessionPage`。
use rust_cli_kernel_api::store::{SessionOrder, SessionQuery};

use super::*;

pub(super) async fn write(dir: &str) -> Result<()> {
    let store = store(dir)?;
    let mut sessions = Vec::new();
    for (root, prompt) in [
        ("/w/manage", "manage the first project"),
        ("/w/manage-other", "manage the second project"),
    ] {
        let id = ids().session();
        store
            .create(SessionMeta {
                id: id.clone(),
                kind: SessionKind::Interactive,
                parent: None,
                workspace: WorkspaceRef {
                    environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
                    root: EnvPath::new(root),
                },
                title: String::new(),
                trace: TraceId::new(id.as_str()),
                created_at_ms: now_ms(),
            })
            .await?;
        store
            .commit(&id, batch(None, vec![user(&message_id(), prompt)]))
            .await?;
        sessions.push(id.as_str().to_owned());
    }
    print(&json!({ "sessions": sessions }))
}

pub(super) async fn rename(dir: &str, session: &str, title: &str) -> Result<()> {
    store(dir)?.rename(&SessionId::new(session), title).await?;
    print(&json!({ "ok": true }))
}

pub(super) async fn delete(dir: &str, session: &str) -> Result<()> {
    store(dir)?.delete(&SessionId::new(session)).await?;
    print(&json!({ "ok": true }))
}

pub(super) async fn query(dir: &str, root: Option<&str>) -> Result<()> {
    let page = store(dir)?
        .query_sessions(SessionQuery {
            environment: EnvironmentId::new(LOCAL_ENVIRONMENT),
            root: root.map(EnvPath::new),
            order: SessionOrder::Updated,
            limit: 50,
        })
        .await?;
    print(&serde_json::to_value(page)?)
}
