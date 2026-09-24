//! Params of legacy `session/send`, `session/compact` and `session/goal`
//! (`zcodeSessionSendParamsSchema`, `zcodeSessionCompactParamsSchema`,
//! `zcodeSessionGoalParamsSchema`).
use super::legacy_params::{
    ParamsError, model, non_negative, optional, parse, required, strings,
};
use super::zod::{Issue, Schema, non_empty, string, string_min};
use serde_json::{Value, json};
use std::sync::OnceLock;

/// `modelExecutionSchema` (`packages/shared/src/model-execution.ts`).
fn model_execution() -> Schema {
    let headers = Schema::Record(Box::new(string_min(1)), Some(Box::new(string_min(1))));
    Schema::Object(vec![
        optional("memoryExtraction", Schema::Literal(json!("skip"))),
        required("selectionScope", Schema::Literal(json!("execution"))),
        optional(
            "requestAuth",
            Schema::Object(vec![
                optional("apiKey", string_min(1)),
                optional("headers", headers),
            ]),
        ),
        optional(
            "subagents",
            Schema::Object(vec![
                required("foregroundModel", Schema::Literal(json!("submission"))),
                required("background", Schema::Literal(json!("deny"))),
            ]),
        ),
    ])
}

/// `zcodeBrowserAmbientContextSchema`.
fn browser_ambient_context() -> Schema {
    Schema::Object(vec![
        required("tabCount", Schema::Int(Some((0, false)), Some(100))),
        optional(
            "currentUrl",
            Schema::String {
                trim: true,
                min: Some(1),
                max: Some(4096),
                format: None,
            },
        ),
    ])
}

/// JS truthiness of an optional parsed field (strings are truthy when non-empty).
fn truthy(value: &Value, key: &str) -> bool {
    match value.get(key) {
        None | Some(Value::Null) => false,
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// The send schema's `superRefine`, on the trimmed values.
fn send_refinements(value: &Value) -> Vec<Issue> {
    let mut issues = vec![];
    if truthy(value, "automationId") && truthy(value, "offPeakTaskId") {
        issues.push(Issue::custom(
            &[],
            "automationId and offPeakTaskId are mutually exclusive",
        ));
    }
    if truthy(value, "offPeakRunType") && !truthy(value, "offPeakTaskId") {
        issues.push(Issue::custom(
            &["offPeakRunType"],
            "offPeakRunType requires offPeakTaskId",
        ));
    }
    if truthy(value, "modelExecution") && !truthy(value, "modelSelection") {
        issues.push(Issue::custom(
            &["modelExecution"],
            "modelExecution requires modelSelection",
        ));
    }
    issues
}

/// `[send, compact, goal]`.
fn schemas() -> &'static [Schema; 3] {
    static SCHEMAS: OnceLock<[Schema; 3]> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let json_object = || Schema::Record(Box::new(string()), None);
        let send = Schema::Object(vec![
            required("sessionId", non_empty()),
            optional("modelSelection", model()),
            optional("modelExecution", model_execution()),
            optional("inputId", non_empty()),
            optional("queryId", non_empty()),
            required("content", string()),
            optional(
                "attachments",
                Schema::Array(Box::new(json_object()), None),
            ),
            optional("browserAmbientContext", browser_ambient_context()),
            optional("expectedRevision", non_negative()),
            optional("expectedProviderRevision", non_empty()),
            optional("automationId", non_empty()),
            optional("offPeakTaskId", non_empty()),
            optional("offPeakRunType", Schema::Enum(&["init", "resume"])),
            optional("toolDenylist", strings()),
        ]);
        [
            Schema::Refined(Box::new(send), send_refinements),
            Schema::Object(vec![
                required("sessionId", non_empty()),
                optional("inputId", non_empty()),
                optional("instructions", string()),
                optional("expectedRevision", non_negative()),
            ]),
            Schema::Object(vec![
                required("sessionId", non_empty()),
                optional("inputId", non_empty()),
                required(
                    "action",
                    Schema::Enum(&["show", "set", "replace", "pause", "resume", "clear"]),
                ),
                optional("objective", string()),
                optional("expectedRevision", non_negative()),
            ]),
        ]
    })
}

pub fn send(params: &Value) -> Result<Value, ParamsError> {
    parse(&schemas()[0], params)
}

pub fn compact(params: &Value) -> Result<Value, ParamsError> {
    parse(&schemas()[1], params)
}

pub fn goal(params: &Value) -> Result<Value, ParamsError> {
    parse(&schemas()[2], params)
}
