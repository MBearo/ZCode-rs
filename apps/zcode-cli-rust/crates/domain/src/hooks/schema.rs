//! Node `HookJSONOutputSchema` (zod3): known keys only, typed values; the
//! result keeps what zod would return.
use super::HookEvent;
use serde_json::{Map, Value};

/// zod3 object parsing: listed keys only; a present key must pass its check.
/// A field check: the kept value, or `None` when the value is invalid.
type Check = fn(&Value) -> Option<Value>;

fn keep(value: &Value, fields: &[(&str, Check)]) -> Option<Value> {
    let object = value.as_object()?;
    let mut out = Map::new();
    for (key, check) in fields {
        if let Some(field) = object.get(*key) {
            out.insert((*key).into(), check(field)?);
        }
    }
    Some(Value::Object(out))
}
fn string(v: &Value) -> Option<Value> {
    v.is_string().then(|| v.clone())
}
fn boolean(v: &Value) -> Option<Value> {
    v.is_boolean().then(|| v.clone())
}
fn any(v: &Value) -> Option<Value> {
    Some(v.clone())
}
fn decision(v: &Value) -> Option<Value> {
    matches!(v.as_str(), Some("approve" | "block")).then(|| v.clone())
}
fn permission_decision(v: &Value) -> Option<Value> {
    matches!(v.as_str(), Some("allow" | "ask" | "deny")).then(|| v.clone())
}
fn event_name(v: &Value) -> Option<Value> {
    v.as_str().and_then(HookEvent::parse).map(|_| v.clone())
}
fn rule(v: &Value) -> Option<Value> {
    v["toolName"].as_str().filter(|t| !t.is_empty())?;
    keep(v, &[("toolName", string), ("ruleContent", string)])
}
fn update(v: &Value) -> Option<Value> {
    (v["type"] == "addRules").then_some(())?;
    let rules = |v: &Value| -> Option<Value> {
        v.as_array()?
            .iter()
            .map(rule)
            .collect::<Option<Vec<_>>>()
            .map(Value::Array)
    };
    keep(
        v,
        &[
            ("type", string),
            ("behavior", permission_decision),
            ("rules", rules),
        ],
    )
    .filter(|u| u.get("behavior").is_some() && u.get("rules").is_some())
}
fn updates(v: &Value) -> Option<Value> {
    v.as_array()?
        .iter()
        .map(update)
        .collect::<Option<Vec<_>>>()
        .map(Value::Array)
}
/// zod3 union: the allow shape first, then deny.
fn request_decision(v: &Value) -> Option<Value> {
    match v["behavior"].as_str() {
        Some("allow") => keep(
            v,
            &[
                ("behavior", string),
                ("permissionUpdates", updates),
                ("updatedPermissions", updates),
                ("updatedInput", any),
            ],
        ),
        Some("deny") => keep(
            v,
            &[
                ("behavior", string),
                ("interrupt", boolean),
                ("message", string),
            ],
        ),
        _ => None,
    }
}
/// zod3 discriminated union on `hookEventName`.
fn specific(v: &Value) -> Option<Value> {
    let event = v["hookEventName"].as_str().and_then(HookEvent::parse)?;
    match event {
        HookEvent::PreToolUse => keep(
            v,
            &[
                ("additionalContext", string),
                ("hookEventName", event_name),
                ("permissionDecision", permission_decision),
                ("permissionDecisionReason", string),
                ("updatedInput", any),
            ],
        ),
        HookEvent::PermissionRequest => keep(
            v,
            &[
                ("decision", request_decision),
                ("hookEventName", event_name),
            ],
        ),
        _ => keep(
            v,
            &[("additionalContext", string), ("hookEventName", event_name)],
        ),
    }
}
/// Node `HookJSONOutputSchema` (zod3, unknown keys stripped).
pub fn validate(value: &Value) -> Option<Value> {
    keep(
        value,
        &[
            ("additionalContext", string),
            ("additional_context", string),
            ("continue", boolean),
            ("decision", decision),
            ("hookSpecificOutput", specific),
            ("reason", string),
            ("stopReason", string),
            ("suppressOutput", boolean),
            ("systemMessage", string),
        ],
    )
}
