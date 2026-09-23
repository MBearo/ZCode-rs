use crate::domain::session_listing::{ListParams, MAX_LIST_BYTES, SessionListing};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params, types::Value as Sql};
use std::collections::BTreeMap;

pub(super) const UPDATED_AT: &str =
    "CASE WHEN json_valid(body) THEN json_extract(body,'$.updatedAt') END";
pub(super) fn prepare(conn: &Connection) -> Result<()> {
    // 索引不能让其他 workspace 的损坏正文阻塞身份查询或单会话恢复。
    conn.execute_batch(&format!("CREATE INDEX IF NOT EXISTS rust_session_listing ON rust_session(workspace,({UPDATED_AT}) DESC,id DESC);
        CREATE INDEX IF NOT EXISTS rust_session_global_listing ON rust_session(({UPDATED_AT}) DESC,id DESC);
        CREATE INDEX IF NOT EXISTS rust_session_id ON rust_session(id);"))?;
    Ok(())
}

const COLUMNS: &str = "json_object('id',id,'workspace',workspace,'workspacePath',json_extract(body,'$.workspacePath'),'promptPath',json_extract(body,'$.promptSnapshot.cwd'),'workspaceDirectory',json_extract(body,'$.workspaceDirectory'),'traceId',json_extract(body,'$.traceId'),'taskType',COALESCE(json_extract(body,'$.taskType'),'interactive'),'title',json_extract(body,'$.title'),'titleSource',json_extract(body,'$.titleSource'),'parentId',json_extract(body,'$.parentId'),'createdAt',json_extract(body,'$.createdAt'),'updatedAt',json_extract(body,'$.updatedAt'),'archivedAt',json_extract(body,'$.archivedAt'))";

pub(super) fn list(
    conn: &Connection,
    p: &ListParams,
    owner: (&str, &str),
) -> Result<Vec<SessionListing>> {
    let mut filter = String::from("COALESCE(json_extract(body,'$.phase'),'')!='draft'");
    let mut args = vec![];
    if let Some(w) = &p.workspace {
        filter.push_str(" AND workspace=?");
        args.push(Sql::Text(w.identity().into()));
    }
    if !p.include_archived {
        filter.push_str(" AND json_extract(body,'$.archivedAt') IS NULL AND COALESCE(json_extract(body,'$.archived'),0)=0");
    }
    let mut records = vec![];
    let mut backups = BTreeMap::new();
    let mut bytes = 16;
    if let Some(ids) = &p.session_ids {
        let mut statement = conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM rust_session WHERE {filter} AND id=?"
        ))?;
        args.push(Sql::Null);
        for id in ids {
            *args.last_mut().unwrap() = Sql::Text(id.clone());
            let mut rows = statement.query(rusqlite::params_from_iter(&args))?;
            if let Some(row) = rows.next()? {
                let record = decode(conn, row.get(0)?, owner, p, &mut backups)?;
                ensure!(
                    rows.next()?.is_none(),
                    "Ambiguous session ID; specify workspace identity"
                );
                push(&mut records, record, p, &mut bytes)?;
            }
        }
    } else {
        filter.push_str(" AND COALESCE(json_extract(body,'$.taskType'),'interactive') IN ('interactive','fork','workflow_parent')");
        // 路径缺失的旧记录要先只读补全，不能在 SQL 中把远端 identity 当文件路径筛掉。
        let limit = p.limit.unwrap_or(50) as usize;
        let mut statement = conn.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM rust_session WHERE {filter} ORDER BY {UPDATED_AT} DESC,id DESC"
        ))?;
        let mut rows = statement.query(rusqlite::params_from_iter(&args))?;
        while let Some(row) = rows.next()? {
            let record = decode(conn, row.get(0)?, owner, p, &mut backups)?;
            if p.workspace.as_ref().is_some_and(|w| {
                record.workspace_directory.as_deref() != Some(w.workspace_path.as_str())
            }) {
                continue;
            }
            push(&mut records, record, p, &mut bytes)?;
            if records.len() >= limit {
                break;
            }
        }
    }
    Ok(records)
}
fn push(
    out: &mut Vec<SessionListing>,
    record: SessionListing,
    p: &ListParams,
    bytes: &mut usize,
) -> Result<()> {
    *bytes += serde_json::to_vec(&record.projection(p.workspace.as_ref()))?.len() + 1;
    ensure!(
        *bytes <= MAX_LIST_BYTES,
        "Session list exceeds frame budget; use a smaller limit or sessionIds batch"
    );
    out.push(record);
    Ok(())
}
fn decode(
    conn: &Connection,
    body: String,
    owner: (&str, &str),
    p: &ListParams,
    backups: &mut BTreeMap<String, Vec<Connection>>,
) -> Result<SessionListing> {
    let mut record: SessionListing = serde_json::from_str(&body)?;
    // 新记录的 traceId=null 是已知事实，不应每次查询都打开旧备份。
    if record.workspace_path.is_none() || record.workspace_directory.is_none() {
        if !backups.contains_key(&record.workspace) {
            let mut snapshots = vec![];
            let imported: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='rust_legacy_import')",
                [],
                |r| r.get(0),
            )?;
            if imported {
                let mut query = conn
                    .prepare_cached("SELECT backup FROM rust_legacy_import WHERE workspace=?1")?;
                let backups = query.query_map([&record.workspace], |r| r.get::<_, String>(0))?;
                for backup in backups {
                    snapshots.push(Connection::open_with_flags(
                        backup?,
                        OpenFlags::SQLITE_OPEN_READ_ONLY,
                    )?);
                }
            }
            backups.insert(record.workspace.clone(), snapshots);
        }
        for snapshot in &backups[&record.workspace] {
            let source: Option<(String,String,Option<String>)>=snapshot.query_row("SELECT COALESCE(NULLIF(path,''),directory),directory,trace_id FROM session WHERE id=?1 AND COALESCE(NULLIF(TRIM(workspace_id),''),directory)=?2",params![record.id,record.workspace],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
            if let Some((path, directory, trace)) = source {
                record.workspace_path.get_or_insert(path);
                record.workspace_directory.get_or_insert(directory);
                if record.trace_id.is_none() {
                    record.trace_id = trace;
                }
                break;
            }
        }
    }
    if record.workspace_path.is_none() {
        record.workspace_path = record.prompt_path.take();
    }
    if record.workspace_path.is_none() {
        record.workspace_path = Some(
            if let Some(w) = p
                .workspace
                .as_ref()
                .filter(|w| w.identity() == record.workspace)
            {
                w.workspace_path.clone()
            } else if record.workspace == owner.0 {
                owner.1.into()
            } else {
                // 缺少远端真实路径时拒绝伪造 workspacePath，调用方可提供原 workspace 定位。
                ensure!(
                    std::path::Path::new(&record.workspace).is_absolute(),
                    "Historical workspace path unavailable"
                );
                record.workspace.clone()
            },
        );
    }
    ensure!(
        !record
            .workspace_path
            .as_deref()
            .context("Workspace path missing")?
            .is_empty(),
        "Workspace path is empty"
    );
    if record.workspace_directory.is_none() {
        record.workspace_directory = record.workspace_path.clone();
    }
    Ok(record)
}
