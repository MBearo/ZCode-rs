use super::legacy_attachments::file_content;
use super::legacy_attempt::check;
use super::legacy_projection::items;
use super::storage::{SessionWrite, write};
use crate::domain::session::Session;
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, params};
use serde_json::{Value, json};
use std::{collections::BTreeMap, path::Path};
use tokio_util::sync::CancellationToken;
pub(super) fn project(
    dest: &Connection,
    snapshot: &Connection,
    workspace: &str,
    cwd: &str,
    dir: &Path,
    artifacts: &Path,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut query=snapshot.prepare("SELECT id,title,title_source,time_created,time_updated,parent_id,revert,time_archived,task_type,trace_id,COALESCE(NULLIF(path,''),directory),directory FROM session WHERE COALESCE(NULLIF(TRIM(workspace_id),''),directory)=?1 ORDER BY time_created,id")?;
    let sessions = query
        .query_map([workspace], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, u64>(3)?,
                r.get::<_, u64>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, Option<String>>(6)?,
                r.get::<_, Option<u64>>(7)?,
                r.get::<_, String>(8)?,
                r.get::<_, Option<String>>(9)?,
                r.get::<_, String>(10)?,
                r.get::<_, String>(11)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (
        id,
        title,
        title_source,
        created,
        updated,
        parent,
        revert,
        archived,
        task_type,
        trace,
        path,
        directory,
    ) in sessions
    {
        check(cancel)?;
        let exists: bool = dest.query_row(
            "SELECT EXISTS(SELECT 1 FROM rust_session WHERE workspace=?1 AND id=?2)",
            params![workspace, id],
            |r| r.get(0),
        )?;
        if exists {
            continue;
        }
        let entries = items(
            snapshot,
            "SELECT type,data FROM session_entry WHERE session_id=?1 ORDER BY time_created,id",
            &id,
        )?;
        let selection = entries
            .iter()
            .rev()
            .find(|(kind, _)| kind == "runtime/model_selection")
            .map(|(_, v)| &v["modelSelection"]);
        let mut session = Session::new(
            id.clone(),
            workspace.into(),
            selection
                .and_then(|s| s["providerId"].as_str())
                .unwrap_or("")
                .into(),
            selection
                .and_then(|s| s["modelId"].as_str())
                .unwrap_or("")
                .into(),
            selection
                .and_then(|s| s["options"]["reasoningLevel"].as_str())
                .unwrap_or("")
                .into(),
            super::id(),
            created,
        );
        session.workspace_path = Some(path);
        session.workspace_directory = Some(directory);
        session.trace_id = trace;
        session.parent_id = parent;
        session.task_type = task_type.clone();
        session.archived_at = archived;
        session.listed = matches!(
            task_type.as_str(),
            "interactive" | "workflow_parent" | "fork"
        );
        session.archived = archived.is_some();
        session.title = title;
        session.title_source = match title_source.as_deref() {
            Some("custom") => "custom",
            Some("generated") => "generated",
            Some("first_input") => "first_input",
            _ => "default",
        }
        .into();
        session.updated_at = updated;
        session.mode = entries
            .iter()
            .rev()
            .find(|(k, _)| k == "runtime/execution_state")
            .and_then(|(_, v)| v["mode"].as_str())
            .unwrap_or("build")
            .into();
        session.plan_enabled = entries
            .iter()
            .rev()
            .find(|(k, _)| k == "runtime/execution_state")
            .is_some_and(|(_, v)| v["planEnabled"] == true || v["mode"] == "plan");
        (session.todos, session.todos_updated_at) =
            super::legacy_todos::read(snapshot, &id, cancel)?;
        session.phase = "completedSuccess".into();
        let messages = items(
            snapshot,
            "SELECT id,data FROM message WHERE session_id=?1 ORDER BY sequence,time_created,id",
            &id,
        )?;
        let mut turn = String::new();
        let mut positions: BTreeMap<String, (usize, usize)> = BTreeMap::new();
        let mut compact_boundary = Value::Null;
        let mut legacy_tail: Option<String> = None;
        let mut interrupted = false;
        let revert: Value = revert
            .as_deref()
            .map(serde_json::from_str)
            .transpose()?
            .unwrap_or(Value::Null);
        for (mid, message) in messages {
            check(cancel)?;
            // TS revert 后隐藏的后缀属于回退源，不进入新的模型上下文。
            if revert["messageID"] == mid && revert["partID"].is_null() {
                break;
            }
            let mut parts = items(
                snapshot,
                "SELECT id,data FROM part WHERE message_id=?1 ORDER BY sequence,time_created,id",
                &mid,
            )?;
            let stop = revert["messageID"] == mid;
            if stop && let Some(part) = revert["partID"].as_str() {
                let end = parts
                    .iter()
                    .position(|(id, _)| id == part)
                    .context("Legacy revert part missing")?;
                parts.truncate(end);
            }
            super::legacy_projection::validate_parts(&parts)?;
            let shared_start = session.messages.len();
            if super::legacy_shared_context::project(&mut session, &message, &parts, &entries, dir)?
            {
                positions.insert(mid, (shared_start, session.messages.len()));
                if stop {
                    break;
                }
                continue;
            }
            for (_, part) in &parts {
                check(cancel)?;
                if part["type"] == "file"
                    && let Some(asset) =
                        super::legacy_attachments::snapshot(part, cwd, artifacts, dir)?
                {
                    session.attachments.insert(
                        part["url"]
                            .as_str()
                            .context("Missing attachment URL")?
                            .into(),
                        asset,
                    );
                }
            }
            let start = session.messages.len();
            for (_, p) in &parts {
                if p["compactBoundary"].is_object() {
                    compact_boundary = p["compactBoundary"].clone();
                }
                if let Some(t) = p["tail_start_id"].as_str() {
                    legacy_tail = Some(t.into());
                }
            }
            let role = message["role"]
                .as_str()
                .context("Invalid legacy message role")?;
            let now = message["time"]["created"].as_u64().unwrap_or(created);
            let text = parts
                .iter()
                .filter(|(_, p)| p["type"] == "text" && p["ignored"] != true)
                .filter_map(|(_, p)| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if role == "user" {
                let visible = message["visibility"] != "model-only"
                    && !matches!(
                        message["semantics"]["uiVisibility"].as_str(),
                        Some("hidden" | "debug")
                    );
                if visible || turn.is_empty() {
                    turn = mid.clone();
                    let mut header = session.row("turnHeader", &turn, &turn, now);
                    header["origin"] = "userInput".into();
                    header["state"] = "completedSuccess".into();
                    header["startedAt"] = now.into();
                    header["endedAt"] = now.into();
                    session.rows.push(header);
                }
                if visible {
                    let mut row = session.row("userInput", &turn, &mid, now);
                    row["text"] = text.clone().into();
                    row["origin"] = if message["synthetic"] == true {
                        "synthetic"
                    } else {
                        "realUser"
                    }
                    .into();
                    if let Some(command) = message["anchor"]["sourceCommandId"].as_str() {
                        row["sourceCommandId"] = command.into();
                    }
                    let attachments=parts.iter().filter(|(_,p)|p["type"]=="file").map(|(_,p)|json!({"ref":p["url"],"fileName":p["filename"].as_str().unwrap_or("attachment"),"mime":p["mime"].as_str().unwrap_or("application/octet-stream"),"bytes":p["metadata"]["sizeBytes"].as_u64().unwrap_or(0)})).collect::<Vec<_>>();
                    if !attachments.is_empty() {
                        row["attachments"] = attachments.into();
                    }
                    session.rows.push(row);
                }
                let mut content = vec![];
                if !text.is_empty() {
                    content.push(json!({"type":"text","text":text}));
                }
                for (_, part) in &parts {
                    if part["type"] == "agent" {
                        content.push(json!({"type":"text","text":format!("[Selected agent: {}]",part["name"].as_str().unwrap_or(""))}));
                    }
                    if part["type"] == "file" {
                        content.extend(file_content(part, cwd, artifacts)?);
                    }
                }
                let content = if content.len() == 1 && content[0]["type"] == "text" {
                    content[0]["text"].clone()
                } else {
                    Value::Array(content)
                };
                if !content.as_array().is_some_and(Vec::is_empty)
                    && message["semantics"]["providerVisibility"] != "hidden"
                {
                    session
                        .messages
                        .push(json!({"role":"user","content":content}));
                }
                if session.provider.is_empty()
                    && let Some(s) = message.get("modelSelection")
                {
                    session.provider = s["providerId"].as_str().unwrap_or("").into();
                    session.model = s["modelId"].as_str().unwrap_or("").into();
                    session.reasoning_level =
                        s["options"]["reasoningLevel"].as_str().unwrap_or("").into();
                }
            } else {
                ensure!(role == "assistant", "Unsupported legacy message role");
                if turn.is_empty() {
                    turn = message["parentID"].as_str().unwrap_or(&mid).into();
                }
                let unfinished = message["time"]["completed"].is_null();
                interrupted |= unfinished;
                let reasoning = parts
                    .iter()
                    .filter(|(_, p)| p["type"] == "reasoning")
                    .filter_map(|(_, p)| p["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                for (pid, p) in &parts {
                    if matches!(p["type"].as_str(), Some("text" | "reasoning"))
                        && p["ignored"] != true
                        && message["summary"] != true
                    {
                        let kind = if p["type"] == "reasoning" {
                            "reasoning"
                        } else {
                            "assistantText"
                        };
                        let mut row = session.row(kind, &turn, pid, now);
                        row["text"] = p["text"].clone();
                        row["assistantResponseId"] = mid.clone().into();
                        row["state"] = if unfinished {
                            "interrupted"
                        } else {
                            "complete"
                        }
                        .into();
                        session.rows.push(row);
                    }
                }
                let mut tools = parts
                    .iter()
                    .filter(|(_, p)| p["type"] == "tool")
                    .collect::<Vec<_>>();
                tools.sort_by_key(|(_, p)| p["declarationIndex"].as_u64().unwrap_or(u64::MAX));
                let mut canonical = json!({"role":"assistant","content":text,"reasoning_content":reasoning,"_zcode_origin":{"provider":message["providerId"].as_str().unwrap_or("unknown"),"model":message["modelId"].as_str().unwrap_or("unknown")}});
                super::legacy_projection::reasoning(&mut canonical, &parts);
                if !tools.is_empty() {
                    canonical["tool_calls"]=tools.iter().map(|(_,p)|json!({"id":p["callID"],"type":"function","function":{"name":p["tool"],"arguments":p["state"]["input"].to_string()}})).collect();
                }
                if message["summary"] != true
                    && message["semantics"]["providerVisibility"] != "hidden"
                {
                    session.messages.push(canonical);
                }
                for (pid, p) in tools {
                    let state = &p["state"];
                    let status = state["status"].as_str().unwrap_or("pending");
                    let output = match status {
                        "completed" => state["output"].as_str().unwrap_or(""),
                        "error" => state["error"].as_str().unwrap_or("Tool failed"),
                        _ => {
                            "Interrupted; execution outcome is unknown. Inspect effects before deciding whether to retry."
                        }
                    };
                    interrupted |= !matches!(status, "completed" | "error");
                    if message["semantics"]["providerVisibility"] != "hidden" {
                        session.messages.push(json!({"role":"tool","tool_call_id":p["callID"],"content":output,"_zcode_tool_failed":status!="completed"}));
                    }
                    let mut row = session.row("toolCall", &turn, pid, now);
                    row["assistantResponseId"] = mid.clone().into();
                    row["toolCallId"] = p["callID"].clone();
                    row["toolName"] = p["tool"].clone();
                    row["inputText"] = state["input"].to_string().into();
                    row["status"] = match status {
                        "completed" => "success",
                        "error" => "error",
                        _ => "cancelled",
                    }
                    .into();
                    row["output"] = json!({"text":output});
                    if status == "error" {
                        row["error"] = json!({"code":"tool.failed","message":output});
                    }
                    session.rows.push(row);
                }
                if message["summary"] == true {
                    session.context.offset = compact_boundary["preservedSegment"]["headMessageId"]
                        .as_str()
                        .or(legacy_tail.as_deref())
                        .and_then(|id| positions.get(id).map(|p| p.0))
                        .or_else(|| {
                            compact_boundary["lastSummarizedMessageId"]
                                .as_str()
                                .and_then(|id| positions.get(id).map(|p| p.1))
                        })
                        .unwrap_or(session.messages.len());
                    session.context.summary = Some(text);
                    let mut row = session.row("timelineMarker", &turn, &mid, now);
                    row["lane"] = "assistantWork".into();
                    row["marker"] = json!({"type":"compact","origin":"auto","status":"success"});
                    session.rows.push(row);
                }
                for (key, target) in [("input", "inputTokens"), ("output", "outputTokens")] {
                    let old = session.usage["cumulative"][target].as_u64().unwrap_or(0);
                    session.usage["cumulative"][target] =
                        (old + message["tokens"][key].as_u64().unwrap_or(0)).into();
                }
            }
            super::legacy_projection::timeline(&mut session, &parts, &turn, now);
            positions.insert(mid, (start, session.messages.len()));
            if stop {
                break;
            }
        }
        super::legacy_projection::ledger(snapshot, &id, &mut session)?;
        super::legacy_shared_context::validate(&session, &entries)?;
        if interrupted {
            session.phase = "completedInterrupted".into();
            session.auto_drain = false;
            for row in &mut session.rows {
                if row["kind"] == "turnHeader" && row["turnId"] == turn {
                    row["state"] = "completedInterrupted".into();
                }
            }
        }
        // 所有原始字段、快照和未实现投影仍完整保留在只读备份，import 的模型事实只追加一次。
        check(cancel)?;
        write(dest, workspace, Some(SessionWrite::new(&session)?), None)?;
    }
    Ok(())
}
