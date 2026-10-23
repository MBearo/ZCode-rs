// 复制自 apps/zcode-cli-rust/crates/state/src/node/sessions.rs 与 session_update.rs（feat/rs 79b6691c55）。
//! `session` 行（Node `repositories/sessions.ts`，docs/specs/rust-m11-node-storage.md §4）。
//!
//! 与复制来源的差异：只保留本 crate 用到的列与操作；`createSession` 的 insert 列与 Node 当前
//! 源码一致（不再列出 `time_archived`）。
use rusqlite::{Connection, OptionalExtension, Row, params, types::Value as Sql};
use rust_cli_base::js::json::encode;
use serde_json::Value;

/// 存储形态的 Node `SessionInfo`（本 crate 读取的列）。
#[derive(Clone, Debug, PartialEq)]
pub struct SessionRow {
    pub id: String,
    pub workspace_id: Option<String>,
    pub parent_id: Option<String>,
    pub trace_id: Option<String>,
    pub task_type: String,
    pub directory: String,
    pub title: String,
    pub revert: Option<Value>,
    pub permission: Option<Value>,
    pub time_created: i64,
    pub time_updated: i64,
    pub time_deleted: Option<i64>,
}

/// Node `CreateSessionInput`（本 crate 写入的成员）。
#[derive(Clone, Debug, Default)]
pub struct Create {
    pub id: String,
    pub project_id: String,
    pub workspace_id: Option<String>,
    pub parent_id: Option<String>,
    pub trace_id: Option<String>,
    pub task_type: Option<String>,
    pub slug: String,
    pub directory: String,
    pub path: Option<String>,
    pub title: String,
    pub title_source: Option<String>,
    pub version: String,
    pub permission: Option<Value>,
    pub time_created: Option<i64>,
}

/// Node `UpdateSessionInput`（本 crate 写入的成员）。
#[derive(Clone, Debug, Default)]
pub struct Update {
    pub id: String,
    pub title: Option<String>,
    pub expected_title_sources: Vec<String>,
    pub title_source: Option<String>,
    /// 生成标题时的首条输入（Node `titleMessageID`）。
    pub title_message_id: Option<String>,
    pub revert: Option<Value>,
    pub permission: Option<Value>,
    /// Node `lastOutcome`：completed / interrupted / error。
    pub last_outcome: Option<String>,
    pub time_updated: Option<i64>,
}

fn json_column(row: &Row, name: &str) -> rusqlite::Result<Option<Value>> {
    let text: Option<String> = row.get(name)?;
    text.map(|t| {
        serde_json::from_str(&t).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .transpose()
}

/// Node `decodeSessionRow`：未知的 task type 回退到 `interactive`。
fn decode(row: &Row) -> rusqlite::Result<SessionRow> {
    let nonempty = |v: Option<String>| v.filter(|s| !s.is_empty());
    let task_type: Option<String> = row.get("task_type")?;
    Ok(SessionRow {
        id: row.get("id")?,
        workspace_id: nonempty(row.get("workspace_id")?),
        parent_id: nonempty(row.get("parent_id")?),
        trace_id: nonempty(row.get("trace_id")?),
        task_type: task_type
            .filter(|t| super::codecs::TASK_TYPES.contains(&t.as_str()))
            .unwrap_or_else(|| "interactive".into()),
        directory: row.get("directory")?,
        title: row.get("title")?,
        revert: json_column(row, "revert")?,
        permission: json_column(row, "permission")?,
        time_created: row.get("time_created")?,
        time_updated: row.get("time_updated")?,
        time_deleted: row.get("time_deleted")?,
    })
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<SessionRow>> {
    conn.prepare_cached("select * from session where id = ?")?
        .query_row([id], decode)
        .optional()
}

/// Node `createSession`（upsert，保留首次的 trace 与 time_created）。
pub fn create(conn: &Connection, input: &Create, now: i64) -> rusqlite::Result<()> {
    let created = input.time_created.unwrap_or(now);
    let updated = created;
    let titled = input.title_source.as_deref().is_some_and(|s| !s.is_empty());
    conn.prepare_cached(
        "insert into session (
        id, project_id, workspace_id, parent_id, trace_id, task_type, slug, directory, path,
        title, title_source, title_message_id, version,
        share_url, summary_additions, summary_deletions, summary_files, summary_diffs,
        revert, permission, time_created, time_updated, time_title_updated,
        time_compacting
      ) values (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, null, null, null, null, null, ?, ?, ?, ?, null)
      on conflict(id) do update set
        project_id = excluded.project_id,
        workspace_id = excluded.workspace_id,
        parent_id = excluded.parent_id,
        trace_id = coalesce(session.trace_id, excluded.trace_id),
        task_type = excluded.task_type,
        slug = excluded.slug,
        directory = excluded.directory,
        path = excluded.path,
        title = excluded.title,
        title_source = excluded.title_source,
        title_message_id = excluded.title_message_id,
        version = excluded.version,
        share_url = excluded.share_url,
        permission = coalesce(excluded.permission, session.permission),
        time_title_updated = excluded.time_title_updated,
        time_updated = excluded.time_updated",
    )?
    .execute(params![
        input.id,
        input.project_id,
        input.workspace_id,
        input.parent_id,
        input.trace_id,
        input.task_type.as_deref().unwrap_or("interactive"),
        input.slug,
        input.directory,
        input.path,
        input.title,
        input.title_source.as_deref().unwrap_or("first_input"),
        Option::<String>::None,
        input.version,
        Option::<String>::None,
        input.permission.as_ref().and_then(encode),
        created,
        updated,
        titled.then_some(updated),
    ])?;
    Ok(())
}

/// Node `updateSession`：只更新本次输入拥有的列；标题编辑不推进活动时间。给出
/// `expected_title_sources` 时按当前标题来源做 CAS。返回改动的行数。
pub fn update(conn: &Connection, input: &Update, now: i64) -> rusqlite::Result<usize> {
    let mut fields = Vec::<String>::new();
    let mut values = Vec::<Sql>::new();
    let mut set = |name: &str, value: Option<Sql>| {
        if let Some(value) = value {
            fields.push(format!("{name} = ?"));
            values.push(value);
        }
    };
    let json = |v: &Value| encode(v).map_or(Sql::Null, Sql::Text);
    set("title", input.title.clone().map(Sql::Text));
    set("title_source", input.title_source.clone().map(Sql::Text));
    set(
        "title_message_id",
        input.title_message_id.clone().map(Sql::Text),
    );
    set("last_outcome", input.last_outcome.clone().map(Sql::Text));
    set("revert", input.revert.as_ref().map(json));
    set("permission", input.permission.as_ref().map(json));
    if input.title.is_some() || input.title_source.is_some() {
        set("time_title_updated", Some(now.into()));
    }
    if let Some(at) = input.time_updated {
        fields.push("time_updated = max(time_updated, ?)".into());
        values.push(at.into());
    }
    if fields.is_empty() {
        return Ok(0);
    }
    values.push(input.id.clone().into());
    let guard = if input.title.is_some() && !input.expected_title_sources.is_empty() {
        values.extend(input.expected_title_sources.iter().cloned().map(Sql::Text));
        format!(
            " and coalesce(title_source, 'first_input') in ({})",
            vec!["?"; input.expected_title_sources.len()].join(",")
        )
    } else {
        String::new()
    };
    conn.prepare_cached(&format!(
        "update session set {} where id=?{guard}",
        fields.join(",")
    ))?
    .execute(rusqlite::params_from_iter(values))
}

/// `/resume` 与 `-c` 的会话列表：本目录的顶层会话加 fork 出的会话（`task_type = 'fork'`），排序同
/// Node `listSessions`（`order by time_updated desc, id desc`）。
///
/// fork 也作为普通会话列出；Node `listZCodeSessions`（`roots: true`）不列 fork，而 Node 的条件只能
/// 并列（`roots` 与 `taskTypes` 是“且”），所以这条查询是本工程的。子代理子会话（`subagent_child`）与
/// App 的 selection side chat 有 `parent_id` 又不是 fork，不列（docs/specs/rust-cli/session-lifecycle.md
/// §6.2）。
pub fn list(
    conn: &Connection,
    directory: &str,
    limit: Option<i64>,
) -> rusqlite::Result<Vec<SessionRow>> {
    let mut clauses: Vec<&str> = vec!["time_deleted is null"];
    let mut values: Vec<Sql> = vec![];
    if !directory.is_empty() {
        clauses.push("directory = ?");
        values.push(Sql::Text(directory.to_owned()));
    }
    clauses.push("(parent_id is null or task_type = 'fork')");
    let limit = limit.filter(|l| *l > 0);
    if let Some(limit) = limit {
        values.push(Sql::Integer(limit));
    }
    let sql = format!(
        "select * from session where {} order by time_updated desc, id desc{}",
        clauses.join(" and "),
        if limit.is_some() { " limit ?" } else { "" }
    );
    conn.prepare_cached(&sql)?
        .query_map(rusqlite::params_from_iter(values), decode)?
        .collect()
}

/// ← 项目页与 `/resume` 页的会话（修复线 F29，docs/specs/rust-cli/node-store.md §5.1）：App 任务列表的口径
/// （Node `sessions-index` 的 `listSessions{directory, taskTypes: TASK_LIST_SESSION_TYPES, workspaceID: null}`），
/// `directory` 为 None 时不限目录（所有项目）。`created` 为真时按创建时间排。
pub fn query(
    conn: &Connection,
    directory: Option<&str>,
    created: bool,
    limit: Option<i64>,
) -> rusqlite::Result<Vec<SessionRow>> {
    let mut clauses: Vec<&str> = vec![
        "time_deleted is null",
        "workspace_id is null",
        "task_type in ('interactive', 'fork', 'workflow_parent')",
    ];
    let mut values: Vec<Sql> = vec![];
    if let Some(directory) = directory {
        clauses.push("directory = ?");
        values.push(Sql::Text(directory.to_owned()));
    }
    if let Some(limit) = limit {
        values.push(Sql::Integer(limit));
    }
    let order = if created {
        "time_created"
    } else {
        "time_updated"
    };
    let sql = format!(
        "select * from session where {} order by {order} desc, id desc{}",
        clauses.join(" and "),
        if limit.is_some() { " limit ?" } else { "" }
    );
    conn.prepare_cached(&sql)?
        .query_map(rusqlite::params_from_iter(values), decode)?
        .collect()
}

/// Node `softDeleteSession`：幂等，保留 message / part 与活动时间。
pub fn soft_delete(conn: &Connection, id: &str, time_deleted: i64) -> rusqlite::Result<()> {
    conn.prepare_cached(
        "update session set time_deleted = coalesce(time_deleted, ?) where id = ?",
    )?
    .execute(params![time_deleted, id])?;
    Ok(())
}

/// Node `touchSession`。
pub fn touch(conn: &Connection, id: &str, time_updated: i64) -> rusqlite::Result<()> {
    conn.prepare_cached("update session set time_updated = max(time_updated, ?) where id = ?")?
        .execute(params![time_updated, id])?;
    Ok(())
}
