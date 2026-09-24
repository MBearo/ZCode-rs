//! Command receipts from durable Node facts (spec rust-m11-node-storage §7):
//! Node `loadPersistentCommandFacts` in `lookupExact` order (transcript,
//! timeline, child, discarded) and `lookupGlobalCreateSessionCommand`.
use super::{entries, inputs};
use anyhow::Result;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

const COMMAND_FACT_ENTRY: &str = "v4/command_fact";
const DISCARDED_ON_RESTART: &str =
    "Input was discarded when the CLI restarted; confirm before resending.";

fn accepted(command: &str) -> Value {
    json!({"commandId": command, "status": "accepted", "revisionAtDecision": 0})
}

fn exists(conn: &Connection, sql: &str, session: &str, command: &str) -> Result<bool> {
    Ok(conn
        .prepare_cached(sql)?
        .query_row(params![session, command], |_| Ok(()))
        .optional()?
        .is_some())
}

/// Node `commandAck`: a stored receipt in the protocol's minimal shape.
fn command_ack(value: &Value) -> Option<&Value> {
    let ack = value.as_object()?;
    ack.get("commandId")?.as_str().filter(|c| !c.is_empty())?;
    matches!(
        ack.get("status")?.as_str()?,
        "accepted" | "rejected" | "failed"
    )
    .then_some(())?;
    ack.get("revisionAtDecision")?.as_f64()?;
    Some(value)
}

/// Node's reason for a terminal ledger row (`cancelledReasonCode`).
fn terminal_reason(status: &str, reason: Option<&str>) -> String {
    if status == "discarded" && reason == Some("session_resumed") {
        return "fault.command.inputDiscardedOnRestart".into();
    }
    match reason {
        Some(r)
            if r.starts_with("fault.") || r.starts_with("proto.") || r.starts_with("guard.") =>
        {
            r.into()
        }
        _ => "fault.command.inputCancelled".into(),
    }
}

fn source_command(input: &inputs::Input) -> Option<&str> {
    ["conversationInputIntent", "intent"]
        .iter()
        .find_map(|key| input.payload[key]["sourceCommandId"].as_str())
        .filter(|id| !id.is_empty())
}

/// The durable receipt of `command` in `session`. Admitted inputs of a session
/// without a live runtime are settled `discarded` first (Node
/// `discardAdmittedOnLoad: !live`).
pub fn lookup(
    conn: &Connection,
    (session, live): (&str, bool),
    command: &str,
    now: i64,
) -> Result<Option<Value>> {
    let transcript = "select 1 from message where session_id = ?
        and (case when json_valid(data) then json_extract(data, '$.anchor.sourceCommandId') end) = ?";
    if exists(conn, transcript, session, command)? {
        return Ok(Some(accepted(command)));
    }
    let timeline = "select 1 from part where session_id = ?
        and (case when json_valid(data) then json_extract(data, '$.type') end) = 'timeline'
        and json_extract(data, '$.sourceCommandId') = ?";
    let facts = entries::list(conn, session, Some(COMMAND_FACT_ENTRY))?;
    let fact = |source: &str| {
        facts.iter().rev().find_map(|e| {
            let ack = command_ack(&e.data["ack"])?;
            (e.data["source"] == source && ack["commandId"] == command).then(|| ack.clone())
        })
    };
    if let Some(ack) = fact("timeline") {
        return Ok(Some(ack));
    }
    if exists(conn, timeline, session, command)? {
        return Ok(Some(accepted(command)));
    }
    if let Some(ack) = fact("child") {
        return Ok(Some(ack));
    }
    let mut found = None;
    for mut input in inputs::list(conn, session, None)? {
        if input.status == "admitted" && !live {
            inputs::settle(
                conn,
                &input.id,
                session,
                "discarded",
                Some("session_resumed"),
                now,
            )?;
            input.status = "discarded".into();
            input.status_reason = Some("session_resumed".into());
        }
        if !matches!(input.status.as_str(), "discarded" | "cancelled")
            || source_command(&input) != Some(command)
        {
            continue;
        }
        let reason = terminal_reason(&input.status, input.status_reason.as_deref());
        let restarted = reason == "fault.command.inputDiscardedOnRestart";
        let mut ack = json!({"commandId": command, "status": "failed", "reasonCode": reason,
            "message": if restarted { DISCARDED_ON_RESTART } else { "Input was cancelled before it entered the transcript." },
            "revisionAtDecision": 0});
        if restarted {
            ack["result"] = json!({"type": "inputDisposition", "delivery": input.delivery});
        }
        found = Some(ack);
    }
    Ok(found)
}

/// Node `lookupGlobalCreateSessionCommand`: a `createSession` receipt through
/// its first input's globally unique ledger row.
pub fn lookup_create(conn: &Connection, command: &str, now: i64) -> Result<Option<Value>> {
    let Some(input) = inputs::get(conn, &format!("queue_{command}"))? else {
        return Ok(None);
    };
    if input.payload["sourceCommandType"] != "createSession"
        || input.payload["conversationInputIntent"]["sourceCommandId"] != command
    {
        return Ok(None);
    }
    Ok(Some(match input.status.as_str() {
        "promoted" => json!({"commandId": command, "status": "accepted", "revisionAtDecision": 0,
            "result": {"type": "createSession", "sessionId": input.session_id}}),
        "admitted" => {
            inputs::settle(
                conn,
                &input.id,
                &input.session_id,
                "discarded",
                Some("session_resumed"),
                now,
            )?;
            json!({"commandId": command, "status": "failed",
                "reasonCode": "fault.command.inputDiscardedOnRestart",
                "message": DISCARDED_ON_RESTART, "revisionAtDecision": 0})
        }
        status => json!({"commandId": command, "status": "failed",
            "reasonCode": terminal_reason(status, input.status_reason.as_deref()),
            "revisionAtDecision": 0}),
    }))
}
