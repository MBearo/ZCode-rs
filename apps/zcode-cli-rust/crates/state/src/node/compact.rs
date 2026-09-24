//! Committing a compaction summary (Node `persistCompactSummary`,
//! `selectPersistedCompactTail`, `buildManualCompactBoundary` and the
//! completed timeline): the preserved tail is selected in the stored active
//! transcript by the runtime's preserved round count.
use super::messages::{save_message, save_part};
use super::{cold, sessions};
use anyhow::{Context, Result};
use rusqlite::Connection;
use serde_json::{Map, Value, json};
use zcode_cli_domain::node_history::{self, Branch, Record, branch::preservable};
use zcode_cli_domain::node_journal::timeline::{self, Compaction, Host};

/// Node `groupByAssistantStartedRounds` of the preservable active messages;
/// the last `groups` rounds.
fn preserved_tail<'a>(
    active: &'a [std::borrow::Cow<'a, Record>],
    groups: usize,
) -> Vec<&'a Record> {
    let mut rounds: Vec<Vec<&Record>> = vec![];
    for record in active.iter().map(AsRef::as_ref).filter(|m| preservable(m)) {
        let assistant = record.info["role"] == "assistant";
        match rounds.last_mut() {
            Some(round) if !assistant => round.push(record),
            _ => rounds.push(vec![record]),
        }
    }
    let skip = rounds.len().saturating_sub(groups);
    rounds.into_iter().skip(skip).flatten().collect()
}

fn save(conn: &Connection, (info, parts): (Value, Vec<Value>), now: i64) -> Result<()> {
    save_message(conn, &info, None, now)?;
    for part in &parts {
        save_part(conn, part, None, now)?;
    }
    Ok(())
}

/// Commits the summary, its reminders and the completed timeline.
pub fn summary(conn: &Connection, session: &str, done: &Value, now: i64) -> Result<()> {
    let c: Compaction = serde_json::from_value(done["compaction"].clone())?;
    let host: Host = serde_json::from_value(done["host"].clone())?;
    let row = sessions::get(conn, session)?.context("Compacted session missing")?;
    let all = cold::records(conn, session)?;
    let branch = Branch::from_revert(row.revert.as_ref());
    let active = node_history::active_messages(&all, &branch, true);
    // Node latestConversationMessageId：最近一条对话消息，时间线宿主不计入。
    let latest = active
        .iter()
        .rev()
        .find(|m| {
            matches!(m.info["role"].as_str(), Some("user" | "assistant"))
                && m.info["semantics"]["kind"] != "timeline_event"
        })
        .map(|m| m.id().to_owned());
    let summary_id = done["summaryMessageId"].as_str().context("summary id")?;
    let groups = done["groupsPreserved"].as_u64().unwrap_or(0) as usize;
    let kept = if groups > 0 {
        preserved_tail(&active, groups)
    } else {
        vec![]
    };
    let mut boundary = Map::new();
    let mut put = |key: &str, value: Value| {
        if !value.is_null() {
            boundary.insert(key.into(), value);
        }
    };
    put("boundaryId", done["boundaryId"].clone());
    put("trigger", c.trigger.clone().into());
    put("phase", c.phase.clone().into());
    put("compactReason", c.reason.clone().into());
    put("summarySource", "model".into());
    put(
        "preCompactTokenCount",
        c.pre_tokens.map_or(Value::Null, Value::from),
    );
    put(
        "postCompactTokenCount",
        done["postCompactTokenCount"].clone(),
    );
    put(
        "truePostCompactTokenCount",
        done["truePostCompactTokenCount"].clone(),
    );
    put("autoCompactThreshold", done["autoCompactThreshold"].clone());
    put(
        "willRetriggerNextTurn",
        done["willRetriggerNextTurn"].clone(),
    );
    put(
        "summarizedMessageCount",
        done["summarizedMessageCount"].clone(),
    );
    put("keptMessageCount", kept.len().into());
    put(
        "lastSummarizedMessageId",
        latest.clone().map_or(Value::Null, Value::from),
    );
    if let (Some(head), Some(tail)) = (kept.first(), kept.last()) {
        put(
            "preservedSegment",
            json!({"anchorMessageId": summary_id,
            "headMessageId": head.id(), "tailMessageId": tail.id()}),
        );
    }
    put("summaryMessageIds", json!([summary_id]));
    put("customInstructions", done["customInstructions"].clone());
    put("traceId", done["traceId"].clone());
    put("turnId", done["turnId"].clone());
    let boundary = Value::Object(boundary);
    let now_ms = now as u64;
    let ids = (
        summary_id,
        done["textPartId"].as_str().unwrap_or(""),
        done["compactionPartId"].as_str().unwrap_or(""),
    );
    let texts = (
        done["content"].as_str().unwrap_or(""),
        done["body"].as_str().unwrap_or(""),
    );
    let (selection, tools) = (&done["selection"], &done["tools"]);
    save(
        conn,
        timeline::summary_records(
            &host,
            ids,
            now_ms,
            texts,
            selection,
            tools,
            (&boundary, &c.operation),
        ),
        now,
    )?;
    for reminder in done["reminders"].as_array().into_iter().flatten() {
        let ids = (
            reminder["messageId"].as_str().unwrap_or(""),
            reminder["partId"].as_str().unwrap_or(""),
        );
        let text = (
            reminder["source"].as_str().unwrap_or(""),
            reminder["content"].as_str().unwrap_or(""),
        );
        save(
            conn,
            timeline::reminder_records(&host, ids, now_ms, text, selection, tools),
            now,
        )?;
    }
    let mut update = json!({"boundaryId": done["boundaryId"], "endedAt": now_ms,
        "postCompactTokenCount": done["postCompactTokenCount"], "replace": true,
        "summaryMessageId": summary_id, "truePostCompactTokenCount": done["truePostCompactTokenCount"]});
    if c.max_attempts > 1 {
        update["attempt"] = 1.into();
        update["maxAttempts"] = c.max_attempts.into();
    }
    if let Some(latest) = latest {
        update["tailStartMessageId"] = latest.into();
    }
    let records = c.records(&host, &c.payload("completed", &update));
    let mut records = records.into_iter();
    save_message(conn, &records.next().context("timeline host")?, None, now)?;
    for part in records {
        save_part(conn, &part, None, now)?;
    }
    Ok(())
}

/// Node `recoverInterruptedCompactTimelines` on resume: a compaction left
/// `started` or `retrying` by a stopped runtime closes as `completed` when
/// its boundary was stored, else `interrupted`. Returns how many closed.
pub fn recover(conn: &Connection, host: &Host, now: i64) -> Result<usize> {
    let all = cold::records(conn, &host.session)?;
    let parts = all.iter().flat_map(|m| m.parts.iter().map(move |p| (m, p)));
    let compaction = |p: &&Value| p["type"] == "compaction";
    let boundaries: std::collections::HashMap<&str, &Value> = parts
        .clone()
        .map(|(_, p)| p)
        .filter(compaction)
        .filter_map(|p| {
            Some((
                p["operationId"].as_str()?,
                p.get("compactBoundary").filter(|b| b.is_object())?,
            ))
        })
        .collect();
    let mut recovered = 0;
    for (message, part) in parts.filter(|(_, p)| compaction(p)) {
        let Some(operation) = part["operationId"].as_str().filter(|o| !o.is_empty()) else {
            continue;
        };
        if !matches!(
            part["timelineStatus"].as_str(),
            Some("started" | "retrying")
        ) {
            continue;
        }
        let boundary = boundaries.get(operation).copied();
        let trigger = part["trigger"].as_str().unwrap_or("manual");
        let (phase, reason) = timeline::defaults(trigger);
        let pick = |key: &str, fallback: &str| {
            part[key]
                .as_str()
                .or_else(|| boundary.and_then(|b| b[key].as_str()))
                .unwrap_or(fallback)
                .to_owned()
        };
        let c = Compaction {
            operation: operation.into(),
            message: part["messageID"].as_str().unwrap_or("").into(),
            part: part["id"].as_str().unwrap_or("").into(),
            trigger: trigger.into(),
            phase: pick("phase", phase),
            reason: pick("compactReason", reason),
            source_command: None,
            started: part["time"]["start"]
                .as_u64()
                .or_else(|| message.info["time"]["created"].as_u64())
                .unwrap_or(0),
            pre_tokens: part["preCompactTokenCount"].as_u64(),
            parent: message.info["parentID"].as_str().unwrap_or("").into(),
            max_attempts: 1,
            custom_instructions: false,
        };
        let field = |key: &str| boundary.map_or(Value::Null, |b| b[key].clone());
        let update = json!({"attempt": part["attempt"], "boundaryId": field("boundaryId"),
            "endedAt": now, "maxAttempts": part["maxAttempts"],
            "postCompactTokenCount": field("postCompactTokenCount"), "replace": true,
            "summaryMessageId": boundary.map_or(Value::Null, |b| b["summaryMessageIds"][0].clone()),
            "tailStartMessageId": field("lastSummarizedMessageId"),
            "truePostCompactTokenCount": field("truePostCompactTokenCount")});
        let status = if boundary.is_some() {
            "completed"
        } else {
            "interrupted"
        };
        let mut records = c.records(host, &c.payload(status, &update)).into_iter();
        save_message(conn, &records.next().context("timeline host")?, None, now)?;
        for record in records {
            save_part(conn, &record, None, now)?;
        }
        recovered += 1;
    }
    Ok(recovered)
}
