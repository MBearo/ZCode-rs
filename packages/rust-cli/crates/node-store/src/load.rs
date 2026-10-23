//! 冷读取：会话行、活动分支上的消息、模型选择与模式；会话列表
//! （docs/specs/rust-cli/node-store.md §5，docs/specs/rust-m11-node-storage.md §6）。
use rusqlite::Connection;
use rust_cli_kernel_api::env::{EnvPath, WorkspaceRef};
use rust_cli_kernel_api::ids::{EnvironmentId, MessageId, SessionId, TraceId, TurnId};
use rust_cli_kernel_api::model::{ModelConnection, ModelSelection};
use rust_cli_kernel_api::permission::ExecutionState;
use rust_cli_kernel_api::store::{
    LoadedSession, SessionKind, SessionMeta, SessionSummary, TurnAnchor,
};
use rust_cli_kernel_api::store::{SessionFilter, StoreError};
use serde_json::Value;

use crate::branch::{Branch, active_messages};
use crate::facts::{live_session, unsupported};
use crate::mode;
use crate::operations;
use crate::records;
use crate::repo::messages;
use crate::repo::sessions::{self, SessionRow};
use crate::state::{self, PERMISSION_MODE_ENTRY};
use crate::{LOCAL_ENVIRONMENT, OrStorage};

/// 冷读取的会话；`checkpoints` 留空，由调用方按 [`crate::rewind::Active`] 补出（Node 写的要读产物目录，不在
/// 数据库线程里做，修复线 F12）。
pub fn load(
    conn: &Connection,
    session: &SessionId,
) -> Result<(LoadedSession, Vec<crate::rewind::Active>), StoreError> {
    let row = live_session(conn, session)?;
    let records = messages::messages(conn, session.as_str()).storage()?;
    let branch = Branch::from_revert(row.revert.as_ref());
    let active = active_messages(&records, &branch, true);
    let model = state::selection(conn, session.as_str())
        .storage()?
        .as_ref()
        .map(kernel_selection)
        .map(|selection| with_connection(conn, session, selection))
        .transpose()?;
    let execution = execution_state(conn, &row)?;
    let loaded = LoadedSession {
        messages: crate::hydrate::hydrate(&active),
        model,
        mode: execution.map(|state| state.mode),
        plan: execution.is_some_and(|state| state.plan),
        meta: meta(&row),
        open_operations: operations::open_operations(conn, session.as_str())?,
        pending_interactions: operations::pending_interactions(conn, session.as_str())?,
        checkpoints: Vec::new(),
        // 库里的当前轮：会话 actor 打开会话时的起点（node-store.md §4，修复线 F6）。
        turn: state::current_user(conn, session.as_str())
            .storage()?
            .map(|(prompt, turn)| TurnAnchor {
                prompt: MessageId::new(prompt),
                turn: turn.map(TurnId::new),
            }),
    };
    Ok((loaded, crate::rewind::active_checkpoints(conn, session)?))
}

pub fn list(conn: &Connection, filter: &SessionFilter) -> Result<Vec<SessionSummary>, StoreError> {
    if filter.workspace.environment.as_str() != LOCAL_ENVIRONMENT {
        return Err(unsupported("sessions of a remote environment"));
    }
    // 本目录的顶层会话与 fork 出的会话，按活动时间倒序（session-lifecycle.md §6.2）。
    let limit = i64::try_from(filter.limit).unwrap_or(i64::MAX);
    let rows = sessions::list(conn, filter.workspace.root.as_str(), Some(limit)).storage()?;
    Ok(rows
        .into_iter()
        .map(|row| SessionSummary {
            id: SessionId::new(row.id),
            title: row.title,
            updated_at_ms: u64::try_from(row.time_updated).unwrap_or(0),
        })
        .collect())
}

fn meta(row: &SessionRow) -> SessionMeta {
    let kind = match row.task_type.as_str() {
        "subagent_child" => SessionKind::Subagent,
        "fork" | "selection_side_chat" => SessionKind::Fork,
        _ => SessionKind::Interactive,
    };
    SessionMeta {
        id: SessionId::new(row.id.as_str()),
        kind,
        parent: row.parent_id.as_deref().map(SessionId::new),
        workspace: WorkspaceRef {
            // 远程会话的 workspace_id 是 Node 的远程 identity；本地为 NULL。
            environment: EnvironmentId::new(
                row.workspace_id.as_deref().unwrap_or(LOCAL_ENVIRONMENT),
            ),
            root: EnvPath::new(row.directory.as_str()),
        },
        title: row.title.clone(),
        // Node 会话都写 traceID；缺失时（极旧的会话）链路 ID 取会话 ID。
        trace: TraceId::new(row.trace_id.as_deref().unwrap_or(&row.id)),
        created_at_ms: u64::try_from(row.time_created).unwrap_or(0),
    }
}

/// Node `ModelSelection` → 内核的模型选择。
fn kernel_selection(selection: &Value) -> ModelSelection {
    let text = |value: &Value| value.as_str().unwrap_or("").to_owned();
    ModelSelection {
        provider: text(&selection["providerId"]),
        model: text(&selection["modelId"]),
        reasoning: selection["options"]["reasoningLevel"]
            .as_str()
            .map(str::to_owned),
        connection: None,
    }
}

/// 团队项目（工作线 F49，node-store.md §5 第 5 条）：`rust-cli/model_connection` 的 `providerId` / `modelId` 与 Node 的
/// 选择相同才用（Node 或 App 之后换过模型时不套用）；三个 ID 都要是去空白后非空的字符串。
fn with_connection(
    conn: &Connection,
    session: &SessionId,
    mut selection: ModelSelection,
) -> Result<ModelSelection, StoreError> {
    let Some(data) =
        state::last_entry(conn, session.as_str(), records::MODEL_CONNECTION_ENTRY).storage()?
    else {
        return Ok(selection);
    };
    let matches = data["providerId"] == selection.provider.as_str()
        && data["modelId"] == selection.model.as_str();
    let connection = serde_json::from_value::<ModelConnection>(data["connection"].clone())
        .ok()
        .filter(|connection| {
            [
                &connection.product_id,
                &connection.organization_id,
                &connection.project_id,
            ]
            .iter()
            .all(|id| !rust_cli_base::js::string::trim(id).is_empty())
        });
    if matches {
        selection.connection = connection.map(Box::new);
    }
    Ok(selection)
}

/// Node 执行状态对应的权限模式与 plan 开关（修复线 F20）；`rust-cli/permission_mode` 记的模式与执行状态一致时用它
/// （区分 default 与 dontAsk），Node 续写改过模式后以 Node 为准。
fn execution_state(
    conn: &Connection,
    row: &SessionRow,
) -> Result<Option<ExecutionState>, StoreError> {
    let Some(execution) = state::execution(conn, row).storage()? else {
        return Ok(None);
    };
    let named = state::last_entry(conn, &row.id, PERMISSION_MODE_ENTRY)
        .storage()?
        .and_then(|data| data["mode"].as_str().and_then(mode::from_entry_name));
    Ok(Some(mode::read(&execution, named)))
}
