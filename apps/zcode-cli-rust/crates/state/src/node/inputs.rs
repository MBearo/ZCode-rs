//! The `session_input` ledger (Node `repositories/session-inputs.ts`):
//! admitted → promoted (with the user message, same transaction) /
//! cancelled / discarded / failed. The caller owns the transaction.
//! Spec rust-m11-node-storage §4, §7.
use super::entries;
use super::json::stringify;
use super::messages::{messages, save_message, save_part};
use anyhow::{Result, bail};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Map, Value, json};

/// Node `SessionInputRecord`.
#[derive(Clone, Debug, PartialEq)]
pub struct Input {
    pub id: String,
    pub session_id: String,
    pub kind: String,
    pub delivery: String,
    pub payload: Value,
    pub admitted_sequence: i64,
    pub promoted_sequence: Option<i64>,
    pub promoted_message_id: Option<String>,
    pub status: String,
    pub status_reason: Option<String>,
    pub time_created: i64,
    pub time_updated: i64,
}

/// Node `decodePayload`: an object over `{text: ""}` (so `text` comes first).
fn decode_payload(raw: &str) -> Value {
    let mut payload = Map::new();
    payload.insert("text".into(), "".into());
    if let Ok(Value::Object(parsed)) = serde_json::from_str::<Value>(raw) {
        payload.extend(parsed);
    }
    Value::Object(payload)
}

fn decode(row: &Row) -> rusqlite::Result<Input> {
    let delivery: String = row.get("delivery")?;
    let status: String = row.get("status")?;
    Ok(Input {
        id: row.get("id")?,
        session_id: row.get("session_id")?,
        kind: row.get("kind")?,
        delivery: if matches!(delivery.as_str(), "startNow" | "guide" | "queue") {
            delivery
        } else {
            "queue".into()
        },
        payload: decode_payload(&row.get::<_, String>("payload")?),
        admitted_sequence: row.get("admitted_sequence")?,
        promoted_sequence: row.get("promoted_sequence")?,
        promoted_message_id: row.get("promoted_message_id")?,
        status: if matches!(
            status.as_str(),
            "admitted" | "promoted" | "cancelled" | "discarded" | "failed"
        ) {
            status
        } else {
            "admitted".into()
        },
        status_reason: row.get("status_reason")?,
        time_created: row.get("time_created")?,
        time_updated: row.get("time_updated")?,
    })
}

/// Node `saveSessionInput`: a re-save under the same id replaces kind,
/// delivery and payload but keeps status, session and admission order.
pub fn save(
    conn: &Connection,
    id: &str,
    session: &str,
    kind: &str,
    delivery: &str,
    payload: &Value,
    now: i64,
) -> Result<()> {
    let payload = if payload.is_null() {
        "{}".to_owned()
    } else {
        stringify(payload)
    };
    conn.prepare_cached(
        "insert into session_input (
        id, session_id, kind, delivery, payload,
        admitted_sequence, status, time_created, time_updated
      )
      values (?, ?, ?, ?, ?,
        (select coalesce(max(admitted_sequence), -1) + 1 from session_input where session_id = ?),
        'admitted', ?, ?)
      on conflict(id) do update set
        kind = excluded.kind,
        delivery = excluded.delivery,
        payload = excluded.payload,
        time_updated = excluded.time_updated",
    )?
    .execute(params![
        id, session, kind, delivery, payload, session, now, now
    ])?;
    Ok(())
}

/// One queue edit (Node `SessionInputPatch`).
#[derive(Clone, Debug, Default)]
pub struct Patch {
    pub id: String,
    pub delivery: Option<String>,
    /// A full `TurnInputIntentMetadata` replacing `payload.intent`.
    pub intent: Option<Value>,
    pub text: Option<String>,
    pub queue_position: Option<i64>,
}

/// Node `patchObject` on `conversationInputIntent`.
fn patch_intent(value: &Value, patch: &Patch) -> Value {
    let Value::Object(current) = value else {
        return value.clone();
    };
    let mut out = current.clone();
    if let Some(text) = &patch.text {
        out.insert("text".into(), text.as_str().into());
    }
    let position = patch.queue_position.or_else(|| {
        patch
            .intent
            .as_ref()
            .and_then(|i| i["queuePosition"].as_i64())
    });
    if patch.queue_position.is_some()
        || patch
            .intent
            .as_ref()
            .is_some_and(|i| !i["queuePosition"].is_null())
    {
        let mut order = match current.get("order") {
            Some(Value::Object(order)) => order.clone(),
            _ => Map::new(),
        };
        order.insert(
            "queuePosition".into(),
            position.map_or(Value::Null, Value::from),
        );
        out.insert("order".into(), Value::Object(order));
    }
    if let Some(intent) = &patch.intent {
        let mut delivery = json!({"requested": intent["requestedDelivery"], "admitted": intent["admittedDelivery"]});
        let fallback = intent["fallbackReasonCode"]
            .as_str()
            .filter(|c| !c.is_empty());
        if let Some(code) = fallback {
            delivery["fallbackReasonCode"] = code.into();
        }
        out.insert("delivery".into(), delivery);
        // 没有回退码时沿用旧值；旧值缺失对应 JS 的 undefined，stringify 时省略。
        let steer = match fallback {
            Some(code) => Some(json!({"state": "fellBack", "reasonCode": code})),
            None => current.get("steer").cloned(),
        };
        if let Some(steer) = steer {
            out.insert("steer".into(), steer);
        }
    }
    Value::Object(out)
}

/// Node `updateSessionInputs`: only `admitted` rows change.
pub fn update(conn: &Connection, session: &str, patches: &[Patch], now: i64) -> Result<()> {
    for patch in patches {
        let row: Option<(String, String)> = conn
            .prepare_cached(
                "select delivery, payload from session_input where id = ? and session_id = ? and status = 'admitted'",
            )?
            .query_row(params![patch.id, session], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?;
        let Some((delivery, payload)) = row else {
            continue;
        };
        let mut payload = decode_payload(&payload);
        if let Some(text) = &patch.text {
            payload["text"] = text.as_str().into();
        }
        if let Some(intent) = payload.get("conversationInputIntent") {
            payload["conversationInputIntent"] = patch_intent(intent, patch);
        }
        if let Some(intent) = &patch.intent {
            payload["intent"] = intent.clone();
        } else if let (Some(position), Some(Value::Object(intent))) =
            (patch.queue_position, payload.get("intent"))
        {
            let mut intent = intent.clone();
            intent.insert("queuePosition".into(), position.into());
            payload["intent"] = Value::Object(intent);
        }
        conn.prepare_cached(
            "update session_input set delivery = ?, payload = ?, time_updated = ? where id = ? and session_id = ? and status = 'admitted'",
        )?
        .execute(params![
            patch.delivery.as_deref().unwrap_or(&delivery),
            stringify(&payload),
            now,
            patch.id,
            session
        ])?;
    }
    Ok(())
}

/// Node `promoteSessionInput`: the user message, its parts, shared context
/// attachment and the ledger row move together (no `admitted` guard).
pub fn promote(
    conn: &Connection,
    id: &str,
    session: &str,
    message: &Value,
    parts: &[Value],
    now: i64,
) -> Result<()> {
    save_message(conn, message, None, now)?;
    for part in parts {
        save_part(conn, part, None, now)?;
    }
    let refs = message["metadata"]["inputIntent"]["sharedContextRefs"].as_array();
    for context in refs
        .into_iter()
        .flatten()
        .filter(|r| r["kind"] == "shared_context_import")
    {
        let Some(context) = context["context_id"].as_str() else {
            continue;
        };
        let entry = entries::list(conn, session, Some("v4/shared_context_import"))?
            .into_iter()
            .find(|e| e.data["contextId"] == context);
        let Some(mut entry) = entry else {
            bail!("shared context import is missing");
        };
        if !matches!(entry.data["status"].as_str(), Some("pending" | "reserved")) {
            bail!("shared context import is no longer attachable");
        }
        entry.time_updated = now;
        entry.data["status"] = "attached".into();
        entry.data["attachedMessageId"] = message["id"].clone();
        entries::save(conn, &entry)?;
        let found = messages(conn, session)?
            .into_iter()
            .find(|m| m.info["metadata"]["contextId"] == context);
        if let Some(mut found) = found {
            let mut metadata = match found.info.get("metadata") {
                Some(Value::Object(m)) => m.clone(),
                _ => Map::new(),
            };
            metadata.insert("sharedContextStatus".into(), "attached".into());
            found.info["metadata"] = Value::Object(metadata);
            save_message(conn, &found.info, None, now)?;
        }
    }
    conn.prepare_cached(
        "update session_input
        set status = 'promoted',
            promoted_message_id = ?,
            promoted_sequence = (select coalesce(max(promoted_sequence), -1) + 1 from session_input where session_id = ?),
            time_updated = ?
        where id = ? and session_id = ?",
    )?
    .execute(params![message["id"].as_str(), session, now, id, session])?;
    Ok(())
}

/// Node `markSessionInputPromoted` (only an `admitted` row).
pub fn mark_promoted(
    conn: &Connection,
    id: &str,
    session: &str,
    message: &str,
    now: i64,
) -> Result<()> {
    conn.prepare_cached(
        "update session_input
      set status = 'promoted',
          promoted_message_id = ?,
          promoted_sequence = (select coalesce(max(promoted_sequence), -1) + 1 from session_input where session_id = ?),
          time_updated = ?
      where id = ? and session_id = ? and status = 'admitted'",
    )?
    .execute(params![message, session, now, id, session])?;
    Ok(())
}

/// Node `settleSessionInput`: the first terminal write wins.
pub fn settle(
    conn: &Connection,
    id: &str,
    session: &str,
    status: &str,
    reason: Option<&str>,
    now: i64,
) -> Result<()> {
    conn.prepare_cached(
        "update session_input set status = ?, status_reason = ?, time_updated = ?
      where id = ? and session_id = ? and status = 'admitted'",
    )?
    .execute(params![status, reason, now, id, session])?;
    Ok(())
}

/// Node `listSessionInputs` (`order by admitted_sequence`).
pub fn list(conn: &Connection, session: &str, status: Option<&str>) -> Result<Vec<Input>> {
    let rows = match status {
        Some(status) => conn
            .prepare_cached(
                "select * from session_input where session_id = ? and status = ? order by admitted_sequence",
            )?
            .query_map(params![session, status], decode)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
        None => conn
            .prepare_cached("select * from session_input where session_id = ? order by admitted_sequence")?
            .query_map(params![session], decode)?
            .collect::<rusqlite::Result<Vec<_>>>()?,
    };
    Ok(rows)
}

/// Node `getSessionInputById` (a global id).
pub fn get(conn: &Connection, id: &str) -> Result<Option<Input>> {
    Ok(conn
        .prepare_cached("select * from session_input where id = ?")?
        .query_row([id], decode)
        .optional()?)
}
