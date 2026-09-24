//! Params of the legacy `session/*` methods (`zcodeSessionCreateParamsSchema`,
//! `zcodeSessionResumeParamsSchema` and the setter schemas).
use super::zod::{Format, Schema, non_empty, protocol_error, string};
use serde_json::{Value, json};
use std::sync::OnceLock;

/// A `-32602` params failure: Node's message and `data` (the serialized ZodError).
#[derive(Debug)]
pub struct ParamsError {
    pub message: String,
    pub data: Value,
}

fn optional(key: &'static str, schema: Schema) -> (&'static str, Schema, bool) {
    (key, schema, true)
}

fn required(key: &'static str, schema: Schema) -> (&'static str, Schema, bool) {
    (key, schema, false)
}

fn strings() -> Schema {
    Schema::Array(Box::new(non_empty()), None)
}

/// `z.number().int().nonnegative()`.
fn non_negative() -> Schema {
    Schema::Int(Some((0, true)))
}

fn workspace() -> Schema {
    Schema::Object(vec![
        required("workspacePath", non_empty()),
        optional("workspaceIdentity", non_empty()),
        optional("remoteSessionId", non_empty()),
        required("workspaceKey", non_empty()),
    ])
}

fn model() -> Schema {
    Schema::Object(vec![
        required("providerId", non_empty()),
        required("modelId", non_empty()),
        optional(
            "options",
            Schema::Object(vec![optional("reasoningLevel", non_empty())]),
        ),
    ])
}

fn mcp_server() -> Schema {
    let entry = || {
        Schema::Array(
            Box::new(Schema::Object(vec![
                required("name", non_empty()),
                required("value", string()),
            ])),
            None,
        )
    };
    let tail = || {
        vec![
            optional("isolation", Schema::Enum(&["session", "workspace"])),
            optional(
                "protocolVersion",
                Schema::Enum(&["legacy", "auto", "2026-07-28"]),
            ),
            optional("timeoutMs", Schema::Int(Some((0, false)))),
        ]
    };
    let oauth = Schema::Union(vec![
        Schema::Object(vec![
            required("type", Schema::Literal(json!("client_credentials"))),
            required("clientId", non_empty()),
            required("clientSecret", non_empty()),
            optional("clientName", non_empty()),
            optional("scope", string()),
        ]),
        Schema::Object(vec![
            required("type", Schema::Literal(json!("authorization_code"))),
            optional("clientId", non_empty()),
            optional("clientSecret", non_empty()),
            optional("clientName", non_empty()),
            optional("redirectPath", non_empty()),
            optional("scope", string()),
        ]),
    ]);
    let mut stdio = vec![
        required("name", non_empty()),
        required("command", non_empty()),
        required("args", Schema::Array(Box::new(string()), None)),
        required("env", entry()),
    ];
    stdio.extend(tail());
    let mut http = vec![
        required("name", non_empty()),
        required("type", Schema::Enum(&["http", "sse"])),
        required("url", non_empty()),
        required("headers", entry()),
        optional("oauth", oauth),
    ];
    http.extend(tail());
    Schema::Union(vec![Schema::Object(stdio), Schema::Object(http)])
}

fn imported_history() -> Schema {
    let trimmed = || non_empty();
    let hex = || Schema::String {
        trim: false,
        min: None,
        format: Some(Format::Hex64),
    };
    let message = Schema::Object(vec![
        required("role", Schema::Enum(&["user", "assistant"])),
        required("content", string()),
        optional("timestamp", non_negative()),
    ]);
    let claude = Schema::Object(vec![
        required("source", Schema::Literal(json!("claudeCode"))),
        optional("title", string()),
        optional("createdAt", non_negative()),
        optional("updatedAt", non_negative()),
        required("messages", Schema::Array(Box::new(message), Some(1))),
    ]);
    let artifact = Schema::Object(vec![
        required("artifactId", trimmed()),
        required("workspaceRelativePath", trimmed()),
    ]);
    let provenance = Schema::Object(vec![
        required("shareId", trimmed()),
        optional("contextId", trimmed()),
        optional(
            "shareUrl",
            Schema::String {
                trim: false,
                min: None,
                format: Some(Format::Url),
            },
        ),
        optional(
            "status",
            Schema::Enum(&["pending", "reserved", "attached", "discarded"]),
        ),
        required("projectionSha256", hex()),
        required("artifactSetSha256", hex()),
        required("formatterVersion", Schema::Literal(json!(1))),
        required("markdownSha256", hex()),
        required(
            "installedArtifacts",
            Schema::Array(Box::new(artifact), None),
        ),
    ]);
    let shared = Schema::Object(vec![
        required("source", Schema::Literal(json!("sharedContext"))),
        required("title", trimmed()),
        optional("createdAt", non_negative()),
        required(
            "markdown",
            Schema::String {
                trim: false,
                min: Some(1),
                format: None,
            },
        ),
        required("provenance", provenance),
    ]);
    Schema::Discriminated(
        "source",
        vec![("claudeCode", claude), ("sharedContext", shared)],
    )
}

fn tail() -> Vec<(&'static str, Schema, bool)> {
    vec![
        optional("mcpServers", Schema::Array(Box::new(mcp_server()), None)),
        optional("toolAllowlist", strings()),
        optional("toolDenylist", strings()),
    ]
}

fn create_schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut fields = vec![
            optional("sessionId", non_empty()),
            required("workspace", workspace()),
            optional("parentSessionId", non_empty()),
            optional("mode", Schema::Enum(MODES)),
            optional("model", model()),
            optional("persistence", Schema::Enum(&["immediate", "deferred"])),
            optional("thoughtLevel", non_empty()),
            optional("titleGenerationEnabled", Schema::Bool),
        ];
        fields.extend(tail());
        fields.push(optional("importedHistory", imported_history()));
        fields.push(optional("offPeakToolEnabled", Schema::Bool));
        fields.push(optional("dynamicWorkflowEnabled", Schema::Bool));
        Schema::Object(fields)
    })
}

fn resume_schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let mut fields = vec![
            required("sessionId", non_empty()),
            optional("workspace", workspace()),
            optional("thoughtLevel", non_empty()),
        ];
        fields.extend(tail());
        fields.push(optional("offPeakToolEnabled", Schema::Bool));
        fields.push(optional("dynamicWorkflowEnabled", Schema::Bool));
        Schema::Object(fields)
    })
}

const MODES: &[&str] = &["plan", "build", "edit", "yolo", "auto"];

/// `zcodeSessionSetModelParamsSchema` / `zcodeSessionSetThoughtLevelParamsSchema` /
/// `zcodeSessionSetModeParamsSchema`, in that order.
fn setter_schemas() -> &'static [Schema; 3] {
    static SCHEMAS: OnceLock<[Schema; 3]> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        [
            Schema::Object(vec![
                required("sessionId", non_empty()),
                required("model", model()),
                optional("expectedRevision", non_negative()),
                optional("persistAsWorkspaceLastUsed", Schema::Bool),
            ]),
            Schema::Object(vec![
                required("sessionId", non_empty()),
                optional("thoughtLevel", non_empty()),
                optional("expectedRevision", non_negative()),
                optional("persistAsWorkspaceLastUsed", Schema::Bool),
            ]),
            Schema::Object(vec![
                required("sessionId", non_empty()),
                required("mode", Schema::Enum(MODES)),
                optional("expectedRevision", non_negative()),
            ]),
        ]
    })
}

/// Node `parseParams`. The transport cannot tell absent params from `null`;
/// both are treated as absent (`received undefined`).
fn parse(schema: &Schema, params: &Value) -> Result<Value, ParamsError> {
    let value = (!params.is_null()).then_some(params);
    let (parsed, issues) = schema.run(value);
    if issues.is_empty() {
        return Ok(parsed);
    }
    let (message, data) = protocol_error(&issues);
    Err(ParamsError { message, data })
}

/// `session/create` params with every string trimmed as the schema says.
pub fn create(params: &Value) -> Result<Value, ParamsError> {
    parse(create_schema(), params)
}

pub fn resume(params: &Value) -> Result<Value, ParamsError> {
    parse(resume_schema(), params)
}

pub fn set_model(params: &Value) -> Result<Value, ParamsError> {
    parse(&setter_schemas()[0], params)
}

pub fn set_thought_level(params: &Value) -> Result<Value, ParamsError> {
    parse(&setter_schemas()[1], params)
}

pub fn set_mode(params: &Value) -> Result<Value, ParamsError> {
    parse(&setter_schemas()[2], params)
}

#[cfg(test)]
#[path = "legacy_params_tests.rs"]
mod tests;
