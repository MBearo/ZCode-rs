//! Plan mode (Node `tool/handlers/plan-mode*.ts`, `runtime-reminders.ts`,
//! `plan-file-continuity.ts`, `interaction-broker.ts` plan approval). Texts and
//! the reminder cadence come from TS through `schema/plan-mode.json`.
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::OnceLock;

pub const ENTER: &str = "EnterPlanMode";
pub const EXIT: &str = "ExitPlanMode";
pub const NOT_APPROVED: &str = "The plan was not approved by the user.";
pub const DENIED: &str = "Permission denied for ExitPlanMode";
pub const TURN_STOP_CANCELLED: &str =
    "Tool cancelled because a previous tool result requested a turn stop.";
pub const NOT_IN_PLAN: &str = "You are not in plan mode. This tool is only for exiting plan mode after writing a plan. If your plan was already approved, continue with implementation.";
/// `PLAN_MODE_MAX_PLAN_CHARS * 4 + 1024`.
pub const PLAN_FILE_MAX_BYTES: u64 = 81_024;
const APPROVAL_LEGACY_QUESTION: &str = "Review this implementation plan.";

#[derive(Deserialize)]
struct Tool {
    description: String,
    parameters: Value,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Texts {
    enter_result: String,
    exit_approved: String,
    exit_empty: String,
    reminder_full: String,
    reminder_sparse: String,
    reminder_exit: String,
}

#[derive(Deserialize)]
struct Data {
    tools: std::collections::BTreeMap<String, Tool>,
    texts: Texts,
}

fn data() -> &'static Data {
    static DATA: OnceLock<Data> = OnceLock::new();
    DATA.get_or_init(|| {
        serde_json::from_str(include_str!("../schema/plan-mode.json")).expect("plan mode data")
    })
}

/// Provider definitions of the two tools (main sessions only).
pub fn definitions() -> Vec<Value> {
    [ENTER, EXIT]
        .iter()
        .map(|name| {
            let tool = &data().tools[*name];
            json!({"type":"function","function":{"name":name,"description":tool.description,"parameters":tool.parameters}})
        })
        .collect()
}

pub fn enter_result() -> &'static str {
    &data().texts.enter_result
}

/// Node `formatExitPlanModeModelContent`.
pub fn exit_result(plan: &str) -> String {
    let plan = super::zod::js_trim(plan);
    if plan.is_empty() {
        return data().texts.exit_empty.clone();
    }
    data().texts.exit_approved.replace("{plan}", plan)
}

/// ExitPlanMode input validation (`plan`: 1..=20000 UTF-16 units, not blank).
pub fn exit_plan(args: &Value) -> Result<&str, &'static str> {
    let plan = args["plan"]
        .as_str()
        .ok_or("plan: Invalid input: expected string")?;
    let units = plan.encode_utf16().count();
    if units == 0 || units > 20_000 || super::zod::js_trim(plan).is_empty() {
        return Err("plan: String must contain at least 1 character(s)");
    }
    Ok(plan)
}

/// Wraps a reminder body like Node `wrapSystemReminderForSource` (nested tags escaped).
pub fn reminder_message(body: &str) -> Value {
    let escaped = escape_nested(body);
    json!({"role":"user","content":format!("<system-reminder>\n{escaped}\n</system-reminder>")})
}

fn escape_nested(body: &str) -> String {
    static NESTED: OnceLock<regex::Regex> = OnceLock::new();
    NESTED
        .get_or_init(|| regex::Regex::new(r"(?i)</?system-reminder\b").unwrap())
        .replace_all(body, |caps: &regex::Captures<'_>| {
            format!("&lt;{}", &caps[0][1..])
        })
        .into_owned()
}

/// What an earlier history entry counts as for the reminder cadence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Entry {
    Reminder,
    RealUser,
    Other,
}

/// Node `buildRuntimeModeReminderBody` with plan on: `None`, or the body to add.
pub fn runtime_reminder(entries: &[Entry]) -> Option<&'static str> {
    let last = entries.iter().rposition(|e| *e == Entry::Reminder);
    if let Some(last) = last {
        let humans = entries[last..]
            .iter()
            .filter(|e| **e == Entry::RealUser)
            .count();
        if humans < 5 {
            return None;
        }
    }
    let count = entries.iter().filter(|e| **e == Entry::Reminder).count() + 1;
    let texts = &data().texts;
    Some(if count % 5 == 1 {
        &texts.reminder_full
    } else {
        &texts.reminder_sparse
    })
}

pub fn exit_reminder() -> &'static str {
    &data().texts.reminder_exit
}

/// Real user message for the cadence (Node `metadata.source === "real_user"`).
pub fn is_real_user(message: &Value) -> bool {
    message["role"] == "user"
        && message.get("_zcode_source").is_none()
        && !message["content"].as_str().is_some_and(|c| {
            c.starts_with("<system-reminder>") || c.starts_with("<task-notification>")
        })
}

/// Node `sanitizePlanFileSessionId`: `plan-<id>.md`, `None` when nothing remains.
pub fn plan_file_name(session_id: &str) -> Option<String> {
    let mut out = String::new();
    let mut replaced = false;
    for c in super::zod::js_trim(session_id).chars() {
        let kept = c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-');
        if kept {
            out.push(c);
        } else if !replaced {
            out.push('-');
        }
        replaced = !kept;
    }
    let out = out.trim_matches('-');
    (!out.is_empty()).then(|| format!("plan-{out}.md"))
}

/// Node `formatPlanFileReference`, wrapped as a reminder message.
pub fn plan_file_reminder(path: &str, content: &str) -> Value {
    let body = [
        format!("A plan file exists from plan mode at: {path}"),
        String::new(),
        "Plan contents:".into(),
        String::new(),
        content.into(),
        String::new(),
        "If this plan is relevant to the current work and not already complete, continue working on it.".into(),
    ]
    .join("\n");
    reminder_message(&body)
}

/// V4 pending-interaction payload of a plan approval (a `userInput`, not a permission).
pub fn approval_payload(call_id: &Value, reason: &str, input: &Value) -> Value {
    json!({
        "kind":"userInput","prompt":reason,"freeText":true,"toolCallId":call_id,"toolName":EXIT,
        "input":input,"schema":{"interaction":"plan_approval","toolName":EXIT},
        "questions":[{"question":reason,"header":"Plan","options":[
            {"value":"approve","label":"Approve","description":"Exit plan mode and start implementation."}]}],
    })
}

#[derive(Debug, PartialEq, Eq)]
pub enum Approval {
    Approve,
    /// Declined; `Some` carries the user's feedback.
    Reject(Option<String>),
}

fn normalize(value: &Value) -> String {
    match value {
        Value::String(s) => super::zod::js_trim(s).to_owned(),
        Value::Array(items) => items
            .iter()
            .filter_map(Value::as_str)
            .map(super::zod::js_trim)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(", "),
        _ => String::new(),
    }
}

/// Node `v4AnswerToPlanApprovalResponse` then `planApprovalResponseToBrokerResult`.
/// `optionId: "approve"` counts as a decline, as in Node.
pub fn map_answer(answer: &Value) -> Approval {
    let content = if let Some(action) = answer["action"].as_str() {
        if action != "accept" {
            return Approval::Reject(None);
        }
        answer.get("content").cloned().unwrap_or_else(|| json!({}))
    } else if matches!(
        answer["optionId"].as_str(),
        Some("allowOnce" | "allowAlways")
    ) {
        json!({"answer":"approve"})
    } else {
        match answer["freeText"].as_str().map(super::zod::js_trim) {
            Some(feedback) if !feedback.is_empty() => json!({"answer": feedback}),
            _ => return Approval::Reject(None),
        }
    };
    let picked = [
        &content["answers"][APPROVAL_LEGACY_QUESTION],
        &content["answer_0"],
        &content["answer"],
    ]
    .into_iter()
    .find(|v| !v.is_null())
    .map(normalize)
    .unwrap_or_default();
    match picked.as_str() {
        "approve" => Approval::Approve,
        "" => Approval::Reject(None),
        _ => Approval::Reject(Some(picked)),
    }
}

#[cfg(test)]
#[path = "plan_mode_tests.rs"]
mod tests;
