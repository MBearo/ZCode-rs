use crate::domain::session::Session;
use anyhow::{Result, ensure};
use rusqlite::Connection;
use serde_json::{Value, json};
pub(super) fn ledger(snapshot: &Connection, id: &str, session: &mut Session) -> Result<()> {
    let mut ledger=snapshot.prepare("SELECT id,payload,status,delivery,promoted_message_id FROM session_input WHERE session_id=?1 ORDER BY admitted_sequence")?;
    let inputs = ledger
        .query_map([id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<String>>(4)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    for (input, payload, status, delivery, promoted) in inputs {
        let payload: Value = serde_json::from_str(&payload)?;
        let command = payload["intent"]["sourceCommandId"]
            .as_str()
            .or_else(|| payload["conversationInputIntent"]["sourceCommandId"].as_str());
        if let Some(command) = command {
            let key = serde_json::to_string(&(Some(id), command))?;
            let accepted = status == "promoted";
            let mut ack = json!({"commandId":command,"status":if accepted{"accepted"}else{"failed"},"revisionAtDecision":0,"result":{"type":if accepted{"inputAccepted"}else{"inputDisposition"},"delivery":delivery}});
            if accepted {
                ack["result"]["inputId"] = promoted.unwrap_or(input).into();
            } else {
                ack["reasonCode"] = "fault.input.discardedOnRestart".into();
            }
            session.pending_acks.insert(key, ack);
        }
    }
    Ok(())
}
pub(super) fn validate_parts(parts: &[(String, Value)]) -> Result<()> {
    for (_, p) in parts {
        ensure!(
            matches!(
                p["type"].as_str(),
                Some(
                    "text"
                        | "reasoning"
                        | "file"
                        | "tool"
                        | "compaction"
                        | "timeline"
                        | "retry"
                        | "step-start"
                        | "step-finish"
                        | "snapshot"
                        | "patch"
                        | "agent"
                        | "subtask"
                )
            ),
            "Unsupported TS history part; preserve source and use TS for this workspace"
        );
    }
    Ok(())
}
pub(super) fn timeline(session: &mut Session, parts: &[(String, Value)], turn: &str, now: u64) {
    for (id, p) in parts {
        let marker = match p["type"].as_str() {
            Some("retry") => Some(
                json!({"type":"retryNotice","attempt":p["attempt"],"reasonCode":"provider.retry"}),
            ),
            Some("timeline") => match p["timelineType"].as_str() {
                Some("model_change") if p["toModel"].is_object() => {
                    let to = &p["toModel"];
                    let mut m = json!({"type":"modelChange","toProvider":to["providerId"],"toModel":to["modelId"],"toThought":to["options"]["reasoningLevel"].as_str().unwrap_or("")});
                    if p["fromModel"].is_object() {
                        m["fromProvider"] = p["fromModel"]["providerId"].clone();
                        m["fromModel"] = p["fromModel"]["modelId"].clone();
                    }
                    Some(m)
                }
                Some("context_compaction") => Some(
                    json!({"type":"compact","origin":if p["trigger"]=="manual"{"manual"}else{"auto"},"status":if p["status"]=="completed"{"success"}else{"cancelled"}}),
                ),
                Some("session_fork") => Some(
                    json!({"type":"forkNotice","parentSessionId":p["parentSessionId"],"parentRowId":0}),
                ),
                Some("goal_verification") => Some(
                    json!({"type":"goalVerify","iteration":p["goalIteration"].as_u64().unwrap_or(0),"outcome":if p["verification"]["passed"]==true{"pass"}else{"notSatisfied"},"detail":p["verification"]["reason"].as_str().unwrap_or("")}),
                ),
                _ => None,
            },
            _ => None,
        };
        if let Some(marker) = marker {
            let mut row = session.row("timelineMarker", turn, id, now);
            row["lane"] = if marker["type"] == "modelChange" {
                "lightBoundary"
            } else {
                "turnTailBoundary"
            }
            .into();
            row["marker"] = marker;
            if let Some(command) = p["sourceCommandId"].as_str() {
                row["sourceCommandId"] = command.into();
            }
            session.rows.push(row);
        }
    }
}

pub(super) fn reasoning(message: &mut Value, parts: &[(String, Value)]) {
    let mut thinking = vec![];
    let mut responses = vec![];
    for (_, p) in parts.iter().filter(|(_, p)| p["type"] == "reasoning") {
        let a = &p["metadata"]["anthropic"];
        if let Some(data) = a["redactedData"].as_str() {
            thinking.push(json!({"type":"redacted_thinking","data":data}));
        } else if let Some(signature) = a["signature"].as_str() {
            thinking.push(json!({"type":"thinking","thinking":p["text"],"signature":signature}));
        }
        let r = &p["metadata"]["openai"];
        if let (Some(id), Some(data)) = (
            r["itemId"].as_str(),
            r["reasoningEncryptedContent"].as_str(),
        ) {
            responses.push(json!({"type":"reasoning","id":id,"encrypted_content":data,"summary":[{"type":"summary_text","text":p["text"]}]}));
        }
    }
    if !thinking.is_empty() {
        message["_zcode_anthropic_thinking"] = thinking.into();
    }
    if !responses.is_empty() {
        message["_zcode_responses_reasoning"] = responses.into();
    }
}

pub(super) fn items(conn: &Connection, sql: &str, id: &str) -> Result<Vec<(String, Value)>> {
    let mut q = conn.prepare(sql)?;
    q.query_map([id], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
    })?
    .map(|r| {
        let (id, data) = r?;
        Ok((id, serde_json::from_str(&data)?))
    })
    .collect()
}
