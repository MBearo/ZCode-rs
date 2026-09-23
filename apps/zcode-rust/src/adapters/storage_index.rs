use crate::contract::StorageCommitFailure;
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub(super) fn index(conn: &Connection, workspace: &str) -> Result<BTreeMap<String, Value>> {
    // 启动只选择 sidebar 字段；历史、prompt、附件和工具结果不跨越 SQLite worker 边界。
    let mut statement = conn.prepare_cached("SELECT id,json_object('sessionId',id,'workspaceId',workspace,'title',json_extract(body,'$.title'),'titleSource',CASE json_extract(body,'$.titleSource') WHEN 'first_input' THEN 'generated' ELSE COALESCE(json_extract(body,'$.titleSource'),'default') END,'phase',CASE json_extract(body,'$.phase') WHEN 'running' THEN 'completedInterrupted' WHEN 'prewarming' THEN 'completedInterrupted' ELSE json_extract(body,'$.phase') END,'createdAt',json_extract(body,'$.createdAt'),'lastActivityAt',json_extract(body,'$.updatedAt'),'parentSessionId',json_extract(body,'$.parentId'),'goalStatus',CASE json_extract(body,'$.goal.status') WHEN 'active' THEN 'paused' WHEN 'verifying' THEN 'paused' ELSE json_extract(body,'$.goal.status') END) FROM rust_session WHERE workspace=?1 AND COALESCE(json_extract(body,'$.archived'),0)=0 AND COALESCE(json_extract(body,'$.listed'),1)=1 AND COALESCE(json_extract(body,'$.phase'),'')!='draft'")?;
    statement
        .query_map([workspace], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .map(|row| {
            let (id, body) = row?;
            let mut summary: Value = serde_json::from_str(&body)?;
            summary["sessionEnded"] = summary["phase"]
                .as_str()
                .is_some_and(|p| p.starts_with("completed"))
                .into();
            summary["hasBackgroundWork"] = false.into();
            summary["pendingInteractionSummary"] = json!({"permissionCount":0,"userInputCount":0});
            if summary["parentSessionId"].is_null() {
                summary.as_object_mut().unwrap().remove("parentSessionId");
            }
            if summary["goalStatus"].is_null() {
                summary.as_object_mut().unwrap().remove("goalStatus");
            }
            Ok((id, summary))
        })
        .collect()
}

pub(super) fn ack(conn: &mut Connection, workspace: &str, key: &str) -> Result<Option<Value>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT ack FROM rust_command WHERE workspace=?1 AND key=?2",
            params![workspace, key],
            |r| r.get(0),
        )
        .optional()?;
    let Some(body) = body else { return Ok(None) };
    let mut ack: Value = serde_json::from_str(&body)?;
    if ack["status"] == "accepted" && ack["result"]["type"] == "inputAccepted" {
        let (session, command): (Option<String>, String) = serde_json::from_str(key)?;
        let started: bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM rust_started WHERE workspace=?1 AND session=?2 AND command=?3) OR EXISTS(SELECT 1 FROM rust_row WHERE workspace=?1 AND session=?2 AND (CASE WHEN json_valid(body) THEN json_extract(body,'$.sourceCommandId') END)=?3 AND json_extract(body,'$.kind') IN ('userInput','turnHeader'))",params![workspace,session,command],|r|r.get(0))?;
        if !started {
            // 旧内联格式尚未拆表时，仅在匹配 session 内检查 row 身份，不加载全部历史到 Rust。
            let inline: bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM rust_session s,json_each(s.body,'$.rows') r WHERE s.workspace=?1 AND s.id=?2 AND json_extract(r.value,'$.sourceCommandId')=?3 AND json_extract(r.value,'$.kind') IN ('userInput','turnHeader'))",params![workspace,session,command],|r|r.get(0))?;
            if !inline {
                ack["status"] = "failed".into();
                ack["reasonCode"] = "fault.input.discardedOnRestart".into();
                ack["result"] =
                    json!({"type":"inputDisposition","delivery":ack["result"]["delivery"]});
                super::storage::commit(conn, workspace, None, Some((key.into(), ack.clone())))
                    .map_err(|e| e.context(StorageCommitFailure))?;
            }
        }
    }
    Ok(Some(ack))
}
