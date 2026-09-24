//! Goal verification facts for the cold synthesis: `goal_verification`
//! timeline parts (the current contract) and legacy
//! `target_completion_verification` session entries, deduplicated by
//! `targetId_goalIteration` (Node `transcript-hydration.ts`).
use super::facts::{js_string, text, truthy};
use super::synth::Synth;
use serde_json::{Value, json};
use std::cmp::Ordering;

/// Node `HydratedGoalVerificationEntry`.
#[derive(Clone, Debug, PartialEq)]
pub struct GoalEntry {
    pub payload: Value,
    pub sequence: Option<f64>,
    pub created: f64,
}

/// Node `GoalVerificationFact`; members are absent when undefined.
#[derive(Clone, Debug, Default)]
pub struct GoalFact {
    pub key: String,
    target_id: Option<Value>,
    verification_id: Option<Value>,
    goal_iteration: Option<Value>,
    pub anchor_message: Option<Value>,
    anchor_turn: Option<Value>,
    status: Option<Value>,
    verification: Option<Value>,
}

/// JS template-literal text of a member (`${value}`).
pub fn template(value: Option<&Value>) -> String {
    match value {
        None => "undefined".into(),
        Some(value) => js_string(value),
    }
}

fn key(target: Option<&Value>, iteration: Option<&Value>, verification: Option<&Value>) -> String {
    match iteration {
        Some(iteration) => format!("{}_{}", template(target), template(Some(iteration))),
        None => template(verification),
    }
}

/// Node `goalVerificationTerminalStatus`.
fn terminal_status(status: Option<&Value>) -> &'static str {
    match status.and_then(Value::as_str) {
        Some("completed") => "completed",
        Some("failed" | "failed_closed") => "failed_closed",
        _ => "cancelled",
    }
}

fn put(map: &mut Value, key: &str, value: Option<&Value>) {
    if let Some(value) = value {
        map[key] = value.clone();
    }
}

/// Node `pushGoalVerificationFact`: a started/terminal pair, once per key.
pub fn push_fact(s: &mut Synth, fact: &GoalFact, turn: Option<&str>) -> bool {
    if !s.goals_emitted.insert(fact.key.clone()) {
        return false;
    }
    let mut base = json!({});
    put(&mut base, "targetId", fact.target_id.as_ref());
    put(&mut base, "verificationId", fact.verification_id.as_ref());
    put(&mut base, "goalIteration", fact.goal_iteration.as_ref());
    let anchor = fact.anchor_message.as_ref().filter(|v| truthy(v));
    put(&mut base, "anchorAssistantMessageId", anchor);
    put(
        &mut base,
        "anchorTurnId",
        fact.anchor_turn.as_ref().filter(|v| truthy(v)),
    );
    let mut started = base.clone();
    started["status"] = "started".into();
    s.push("target_completion_verification", started, turn, None);
    let mut terminal = base;
    terminal["status"] = terminal_status(fact.status.as_ref()).into();
    put(
        &mut terminal,
        "verification",
        fact.verification.as_ref().filter(|v| truthy(v)),
    );
    s.push("target_completion_verification", terminal, turn, None);
    true
}

/// Node `synthesizeGoalVerificationPart`: whether `part` was a goal verification.
pub fn goal_part(s: &mut Synth, part: &Value, turn: &str) -> bool {
    if part["timelineType"] != "goal_verification" {
        return false;
    }
    let member = |key: &str| part.get(key).cloned();
    let truthy_member = |key: &str| part.get(key).filter(|v| truthy(v));
    let fact = GoalFact {
        key: key(
            part.get("targetId"),
            part.get("goalIteration"),
            part.get("verificationId"),
        ),
        target_id: member("targetId"),
        verification_id: member("verificationId"),
        goal_iteration: member("goalIteration"),
        anchor_message: truthy_member("anchorMessageId").map(|v| js_string(v).into()),
        anchor_turn: truthy_member("anchorTurnId").map(|v| js_string(v).into()),
        status: truthy_member("status").cloned(),
        verification: truthy_member("verification").cloned(),
    };
    push_fact(s, &fact, Some(turn));
    true
}

/// Node `goalVerificationEntriesFromSessionEntries`: `(data, time.created)`
/// of the stored entries, validated and ordered by event sequence.
pub fn entries(stored: &[(Value, f64)]) -> Vec<GoalEntry> {
    let mut parsed: Vec<GoalEntry> = stored
        .iter()
        .filter_map(|(data, created)| {
            let data = data.as_object()?;
            let payload = data.get("payload")?.as_object()?;
            let target = payload
                .get("targetId")?
                .as_str()
                .filter(|s| !s.is_empty())?;
            let verification = payload
                .get("verificationId")?
                .as_str()
                .filter(|s| !s.is_empty())?;
            let mut out = json!({"targetId": target, "verificationId": verification});
            let typed =
                |key: &str, check: fn(&Value) -> bool| payload.get(key).filter(|v| check(v));
            put(&mut out, "status", typed("status", Value::is_string));
            put(
                &mut out,
                "goalIteration",
                typed("goalIteration", Value::is_number),
            );
            put(
                &mut out,
                "anchorAssistantMessageId",
                typed("anchorAssistantMessageId", Value::is_string),
            );
            put(
                &mut out,
                "anchorTurnId",
                typed("anchorTurnId", Value::is_string),
            );
            put(&mut out, "verification", payload.get("verification"));
            Some(GoalEntry {
                payload: out,
                sequence: data.get("sequenceNumber").and_then(Value::as_f64),
                created: *created,
            })
        })
        .collect();
    parsed.sort_by(|left, right| {
        let order = left.sequence.unwrap_or(left.created) - right.sequence.unwrap_or(right.created);
        order.partial_cmp(&0.0).unwrap_or(Ordering::Equal)
    });
    parsed
}

fn entry_fact(entry: &GoalEntry) -> GoalFact {
    let payload = &entry.payload;
    let member = |key: &str| payload.get(key).cloned();
    GoalFact {
        key: key(
            payload.get("targetId"),
            payload.get("goalIteration"),
            payload.get("verificationId"),
        ),
        target_id: member("targetId"),
        verification_id: member("verificationId"),
        goal_iteration: member("goalIteration"),
        anchor_message: text(&payload["anchorAssistantMessageId"]).map(Value::from),
        anchor_turn: text(&payload["anchorTurnId"]).map(Value::from),
        status: text(&payload["status"]).map(Value::from),
        verification: member("verification"),
    }
}

/// Node `mergeGoalVerificationEntryFacts`: one fact per key in first-seen
/// order; later lifecycle states win, the first anchor is kept.
pub fn merge_entry_facts(entries: &[GoalEntry]) -> Vec<GoalFact> {
    let mut facts: Vec<GoalFact> = Vec::new();
    for entry in entries {
        let fact = entry_fact(entry);
        let Some(existing) = facts.iter_mut().find(|f| f.key == fact.key) else {
            facts.push(fact);
            continue;
        };
        let non_null = |v: &Option<Value>| v.clone().filter(|v| !v.is_null());
        *existing = GoalFact {
            key: fact.key,
            target_id: fact.target_id.or(existing.target_id.take()),
            verification_id: fact.verification_id.or(existing.verification_id.take()),
            goal_iteration: fact.goal_iteration.or(existing.goal_iteration.take()),
            anchor_message: existing.anchor_message.take().or(fact.anchor_message),
            anchor_turn: existing.anchor_turn.take().or(fact.anchor_turn),
            status: fact.status.or(existing.status.take()),
            verification: non_null(&fact.verification).or_else(|| existing.verification.take()),
        };
    }
    facts
}
