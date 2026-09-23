//! V4 command admission validation, equivalent to Node `parseCommandEnvelope`.
//!
//! `schema/v4-command.json` is generated from the TS zod contract by
//! `scripts/generate-zcode-cli-rust-protocol-schema.mjs` and checked for drift
//! in the test suite, so envelope and payload constraints have a single source.
//! Cross-field `superRefine` rules are not representable in JSON Schema and are
//! mirrored in [`refinements`].
use crate::json_schema::Node as Validator;
use serde_json::{Value, json};
use std::{
    collections::{HashMap, HashSet},
    sync::OnceLock,
};

const SCHEMA: &str = include_str!("../schema/v4-command.json");

struct Validators {
    envelope: Validator,
    payloads: HashMap<String, Validator>,
    requires_base_revision: HashSet<String>,
    requires_base_log_epoch: HashSet<String>,
}

fn validators() -> &'static Validators {
    static VALIDATORS: OnceLock<Validators> = OnceLock::new();
    VALIDATORS.get_or_init(|| {
        let schema: Value = serde_json::from_str(SCHEMA).expect("generated command schema is JSON");
        let compile = |schema: &Value| {
            Validator::compile(schema).expect("generated command schema uses the supported subset")
        };
        let names = |key: &str| {
            schema[key]
                .as_array()
                .expect("generated command schema lists")
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        };
        Validators {
            envelope: compile(&schema["envelope"]),
            payloads: schema["payloads"]
                .as_object()
                .expect("generated command payloads")
                .iter()
                .map(|(kind, schema)| (kind.clone(), compile(schema)))
                .collect(),
            requires_base_revision: names("requiresBaseRevision"),
            requires_base_log_epoch: names("requiresBaseLogEpoch"),
        }
    })
}

fn first_issue(validator: &Validator, value: &Value, path: &str) -> Option<String> {
    validator.validate(value, path).err()
}

fn refinements(kind: &str, p: &Value) -> Result<(), String> {
    if kind != "sendText" {
        return Ok(());
    }
    let present = |key: &str| p.get(key).is_some_and(|v| !v.is_null());
    if present("automationId") && present("offPeakTaskId") {
        return Err("(root): automationId and offPeakTaskId are mutually exclusive".into());
    }
    if present("offPeakRunType") && !present("offPeakTaskId") {
        return Err("offPeakRunType: offPeakRunType requires offPeakTaskId".into());
    }
    if present("modelExecution") && !present("modelSelection") {
        return Err("modelExecution: modelExecution requires modelSelection".into());
    }
    Ok(())
}

/// Validate a raw `v4/command` envelope and its payload. Returns the first issue.
pub fn validate_command(raw: &Value) -> Result<(), String> {
    let v = validators();
    if let Some(issue) = first_issue(&v.envelope, raw, "") {
        return Err(issue);
    }
    let kind = raw["type"].as_str().unwrap_or_default();
    let payload = raw.get("payload").unwrap_or(&Value::Null);
    let validator = v
        .payloads
        .get(kind)
        .ok_or_else(|| format!("type: unknown command {kind}"))?;
    if let Some(issue) = first_issue(validator, payload, "payload") {
        return Err(issue);
    }
    refinements(kind, payload)?;
    let missing = |key: &str| raw.get(key).is_none_or(Value::is_null);
    if (v.requires_base_revision.contains(kind) && missing("baseRevision"))
        || (v.requires_base_log_epoch.contains(kind) && missing("baseLogEpoch"))
    {
        let field = if missing("baseRevision") {
            "baseRevision"
        } else {
            "baseLogEpoch"
        };
        return Err(format!(
            "{field}: CAS commands require baseRevision and baseLogEpoch"
        ));
    }
    Ok(())
}

/// ACK for a command that fails admission validation (Node `CommandInbox.handle`).
pub fn invalid_payload_ack(raw: &Value, message: &str) -> Value {
    json!({
        "commandId": raw["commandId"].as_str().unwrap_or(""),
        "status": "rejected",
        "reasonCode": "proto.invalidPayload",
        "message": message,
        "revisionAtDecision": 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn envelope(kind: &str, payload: Value) -> Value {
        json!({"commandId":"c1","clientId":"app","sessionId":"s","type":kind,"payload":payload,"issuedAt":1})
    }

    #[test]
    fn every_generated_command_compiles() {
        assert_eq!(validators().payloads.len(), 34);
    }

    #[test]
    fn accepts_valid_and_rejects_invalid_payloads_like_node() {
        assert!(validate_command(&envelope("sendText", json!({"text":"hi"}))).is_ok());
        // 非 strict 对象的未知字段与 zod 一样被接受（Node 在解析时丢弃）。
        assert!(validate_command(&envelope("sendText", json!({"text":"hi","extra":1}))).is_ok());
        assert!(validate_command(&envelope("sendText", json!({"text":1}))).is_err());
        assert!(validate_command(&envelope("setFollowupMode", json!({"mode":"x"}))).is_err());
        assert!(validate_command(&envelope("nope", json!({}))).is_err());
        let missing_revision = validate_command(&envelope("setAutoDrain", json!({"autoDrain":true})));
        assert!(missing_revision.unwrap_err().starts_with("baseRevision:"));
    }

    #[test]
    fn mirrors_send_text_refinements() {
        let execution = json!({"text":"x","modelExecution":{"selectionScope":"execution"}});
        assert!(validate_command(&envelope("sendText", execution)).is_err());
        let both = json!({"text":"x","automationId":"a","offPeakTaskId":"o"});
        assert!(validate_command(&envelope("sendText", both)).is_err());
        let run_type = json!({"text":"x","offPeakRunType":"init"});
        assert!(validate_command(&envelope("sendText", run_type)).is_err());
    }

    #[test]
    fn invalid_ack_matches_node_shape() {
        let ack = invalid_payload_ack(&json!({"commandId":7}), "bad");
        assert_eq!(
            ack,
            json!({"commandId":"","status":"rejected","reasonCode":"proto.invalidPayload","message":"bad","revisionAtDecision":0})
        );
    }
}
