//! 本机项目的持久模式（docs/specs/rust-cli/config.md §2.2）：Node `repositories/local-settings.ts` 的
//! `getProjectPermissionMode` / `saveProjectPermissionMode`。
//!
//! 值存在 `local_setting(scope='project', scope_id=<projectId>, namespace='permission', key='mode')`，
//! 是 `{"mode": <Node 模式>}`。本工程写入时另带内核的模式名（`rust-cli/permission_mode`），用来
//! 区分 default 与 dontAsk；它只在与 `mode` 一致时有效，Node 整份改写后自然消失。

use rusqlite::{Connection, OptionalExtension, params};
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::store::StoreError;
use serde_json::{Map, Value};

use crate::OrStorage;
use crate::facts::Commit;
use crate::mode::{self, Execution};
use crate::project_rules::{session_project, write_setting};
use crate::state::PERMISSION_MODE_ENTRY;

/// Node `isCollaborationMode` 接受的值。
const NODE_MODES: [&str; 6] = ["plan", "build", "edit", "yolo", "guarded", "auto"];

/// 读本项目的持久模式；没有这一行、值不是 JSON 或模式不合法时为 None（Node 相同）。
pub(crate) fn read(conn: &Connection, project: &str) -> Result<Option<PermissionMode>, StoreError> {
    let text: Option<String> = conn
        .prepare_cached(
            "select value from local_setting where scope = ? and scope_id = ? and namespace = ? and key = ?",
        )
        .storage()?
        .query_row(params!["project", project, "permission", "mode"], |row| {
            row.get(0)
        })
        .optional()
        .storage()?;
    Ok(text.as_deref().and_then(decode))
}

fn decode(text: &str) -> Option<PermissionMode> {
    let value: Value = serde_json::from_str(text).ok()?;
    let node = value
        .get("mode")?
        .as_str()
        .filter(|node| NODE_MODES.contains(node))?;
    // 与会话的模式同一套映射（mode.rs）：Node 旧版写的 `plan` 读成单值的 plan（新会话 plan 开、default，
    // 修复线 F20），guarded 是 guarded。
    let state = mode::resolve(Some(node), &Execution::default());
    let named = value
        .get(PERMISSION_MODE_ENTRY)
        .and_then(Value::as_str)
        .and_then(mode::from_entry_name);
    let read = mode::read(&state, named);
    Some(if read.plan {
        PermissionMode::Plan
    } else {
        read.mode
    })
}

/// 在提交事务里写本会话所在项目的持久模式（Node `saveProjectPermissionMode`，整份替换）。Node 群协作会话
/// （`permission.scope = session`）不写：Node `setMode` 的群模式属于任务授权，不能成为工作区的缺省（修复线 F2）。
pub(crate) fn write(ctx: &Commit<'_>, mode: PermissionMode) -> Result<(), StoreError> {
    let row = crate::repo::sessions::get(ctx.conn, ctx.session).storage()?;
    if row
        .as_ref()
        .and_then(|row| crate::project_rules::scoped(row.permission.as_ref()))
        .is_some()
    {
        return Ok(());
    }
    let project = session_project(ctx)?;
    let mut value = Map::new();
    value.insert("mode".into(), mode::permission_value(mode).into());
    value.insert(PERMISSION_MODE_ENTRY.into(), mode::entry_name(mode).into());
    write_setting(ctx, &project, "mode", &Value::Object(value))
}

#[cfg(test)]
#[path = "project_mode_tests.rs"]
mod tests;
