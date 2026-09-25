//! Tool, permission and subagent facts (Node `ConversationTelemetryFactNormalizer`).
use super::{Event, Normalizer, Runtime, text};
use serde_json::{Map, Value};

const SKILL_SOURCES: [&str; 5] = ["agents", "zcode", "bundled", "plugin", "remote"];
/// Node `toToolPerformanceFact`: `(fact member, detail member)` of a command.
const COMMAND: [(&str, &str); 10] = [
    ("commandRunMs", "runMs"),
    ("firstOutputMs", "firstOutputMs"),
    ("noOutputMs", "noOutputMs"),
    ("exitCode", "exitCode"),
    ("timedOut", "timedOut"),
    ("outputBytes", "outputBytes"),
    ("commandCategory", "category"),
    ("commandName", "name"),
    ("commandCount", "count"),
    ("commandStatus", "status"),
];
const FILESYSTEM: [(&str, &str); 6] = [
    ("fsReadMs", "readMs"),
    ("fsWriteMs", "writeMs"),
    ("fileCount", "fileCount"),
    ("totalBytes", "totalBytes"),
    ("maxFileBytes", "maxFileBytes"),
    ("workspaceKind", "workspaceKind"),
];
const PATCH: [(&str, &str); 3] = [
    ("patchMatchMs", "matchMs"),
    ("hunkCount", "hunkCount"),
    ("matchAttempts", "matchAttempts"),
];

/// Node `streamingParentToolCallId`.
pub(super) fn streaming_parent(payload: &Value) -> Option<&str> {
    let meta = &payload["_meta"];
    [
        &payload["parentToolCallId"],
        &payload["parentToolUseId"],
        &meta["parentToolCallId"],
        &meta["parentToolUseId"],
        &meta["zcode"]["parentToolCallId"],
        &meta["zcode"]["parentToolUseId"],
    ]
    .into_iter()
    .find_map(text)
}

/// Node `mirroredSubagentToolFields`: the child relation of a mirrored call.
fn mirrored(fact: &mut Map<String, Value>, payload: &Value, display: &Value) {
    for key in [
        "parentToolCallId",
        "childToolCallId",
        "agentId",
        "agentType",
        "childSessionId",
    ] {
        if let Some(value) = text(&payload[key]).or_else(|| text(&display[key])) {
            fact.insert(key.into(), value.into());
        }
    }
    if payload["background"] == true {
        fact.insert("background".into(), true.into());
    }
}

/// Node `toToolPerformanceFact`: an explicit whitelist of the tool's `perf`.
fn performance(perf: &Value) -> Option<Value> {
    let mut fact = Map::new();
    for key in ["totalMs", "permissionWaitMs"] {
        if let Some(value) = perf.get(key).filter(|v| !v.is_null()) {
            fact.insert(key.into(), value.clone());
        }
    }
    let detail = &perf["detail"];
    let kind = detail["kind"].as_str().unwrap_or("");
    let mut take = |source: &Value, members: &[(&str, &str)]| {
        for (to, from) in members {
            if let Some(value) = source.get(*from).filter(|v| !v.is_null()) {
                fact.insert((*to).into(), value.clone());
            }
        }
    };
    if kind == "command" {
        take(&detail["command"], &COMMAND);
    }
    if matches!(kind, "filesystem" | "patch") {
        take(&detail["filesystem"], &FILESYSTEM);
    }
    if kind == "patch" {
        take(&detail["patch"], &PATCH);
    }
    (!fact.is_empty()).then_some(Value::Object(fact))
}

/// `automation.automationId` of a CronCreate result.
fn cron_automation(content: &Value) -> Option<String> {
    let parsed: Value = match content {
        Value::String(text) => serde_json::from_str(text).ok()?,
        other => other.clone(),
    };
    text(&parsed["automation"]["automationId"]).map(str::to_owned)
}

impl Normalizer {
    pub(super) fn tool(
        &mut self,
        event: &Event,
        runtime: &Runtime,
        (command, key): (Option<&str>, Option<&str>),
    ) -> Option<Map<String, Value>> {
        let p = event.payload;
        let call = match &p["toolCallId"] {
            Value::String(id) => id.clone(),
            other => other.to_string(),
        };
        let name_key = format!("{}\0{call}", key.unwrap_or(event.session));
        if event.kind == "tool_call_scheduled" {
            let name = p["toolName"].as_str().unwrap_or("");
            self.tool_names.set(name_key, name.into());
            let mut fact = Self::base(event, runtime, command, "tool.lifecycle");
            fact.insert("phase".into(), "scheduled".into());
            fact.insert("toolCallId".into(), call.into());
            fact.insert("toolName".into(), name.into());
            mirrored(&mut fact, p, &Value::Null);
            return Some(fact);
        }
        let name = match p.get("toolName") {
            Some(name) => text(name).map(str::to_owned),
            None => self.tool_names.get(&name_key).cloned(),
        };
        let result = (event.kind == "tool_call_result").then(|| &p["result"]);
        let error = (event.kind == "tool_call_error").then(|| &p["error"]);
        let phase = match event.kind {
            "tool_call_started" => "started",
            "tool_call_progress" => "progress",
            "tool_call_result" if result.is_some_and(|r| r["success"] == false) => "failed",
            "tool_call_result" => "completed",
            _ => "failed",
        };
        if matches!(phase, "completed" | "failed") {
            self.tool_names.remove(&name_key);
        }
        let duration = &p["duration"];
        if result.is_some() && duration.as_f64().is_some_and(|d| d < 0.0) {
            return None;
        }
        let mut fact = Self::base(event, runtime, command, "tool.lifecycle");
        fact.insert("phase".into(), phase.into());
        fact.insert("toolCallId".into(), call.into());
        if let Some(name) = &name {
            fact.insert("toolName".into(), name.as_str().into());
        }
        if phase == "completed"
            && name.as_deref() == Some("CronCreate")
            && let Some(id) = result.and_then(|r| cron_automation(&r["content"]))
        {
            fact.insert("automationId".into(), id.into());
        }
        if result.is_some() {
            fact.insert("durationMs".into(), duration.clone());
        }
        let failure = error.or_else(|| result.map(|r| &r["error"]).filter(|e| e.is_object()));
        if let Some(failure) = failure {
            let code = failure
                .get("code")
                .filter(|c| !c.is_null())
                .unwrap_or(&failure["type"]);
            fact.insert("errorCode".into(), code.clone());
            fact.insert("errorMessage".into(), failure["message"].clone());
        }
        if name.as_deref() == Some("Skill") {
            let skill = &p["skillMetadata"];
            for (to, from) in [
                ("skillQualifiedName", "qualifiedName"),
                ("skillPluginId", "pluginId"),
                ("skillSource", "source"),
            ] {
                if let Some(value) = text(&skill[from]) {
                    if to == "skillSource" && !SKILL_SOURCES.contains(&value) {
                        return None;
                    }
                    fact.insert(to.into(), value.into());
                }
            }
        }
        let display = result.map_or(&Value::Null, |r| &r["display"]);
        mirrored(&mut fact, p, display);
        if let Some(perf) = result.and_then(|r| performance(&r["perf"])) {
            fact.insert("performance".into(), perf);
        }
        Some(fact)
    }
}

pub(super) fn permission(
    event: &Event,
    runtime: &Runtime,
    command: Option<&str>,
) -> Map<String, Value> {
    let p = event.payload;
    let phase = match event.kind {
        "permission_requested" => "requested",
        "permission_resolved" => "resolved",
        _ => "denied",
    };
    let mut fact = Normalizer::base(event, runtime, command, "permission.lifecycle");
    fact.insert("phase".into(), phase.into());
    if phase != "denied"
        && let Some(request) = text(&p["requestId"])
    {
        fact.insert("requestId".into(), request.into());
    }
    fact.insert("toolCallId".into(), p["toolCallId"].clone());
    if phase != "resolved"
        && let Some(name) = text(&p["toolName"])
    {
        fact.insert("toolName".into(), name.into());
    }
    if let Some(child) = text(&p["childSessionId"]) {
        fact.insert("childSessionId".into(), child.into());
    }
    if p["background"] == true {
        fact.insert("background".into(), true.into());
    }
    if phase == "resolved" {
        fact.insert("decision".into(), p["decision"].clone());
    }
    fact
}

pub(super) fn subagent(
    event: &Event,
    runtime: &Runtime,
    command: Option<&str>,
) -> Option<Map<String, Value>> {
    let p = event.payload;
    let agent = text(&p["agentId"])?;
    let child = text(&p["childSessionId"])?;
    let spawned = event.kind == "subagent_spawned";
    let mut fact = Normalizer::base(event, runtime, command, "subagent.lifecycle");
    fact.insert(
        "phase".into(),
        if spawned { "spawned" } else { "stopped" }.into(),
    );
    fact.insert("agentId".into(), agent.into());
    if let Some(kind) = text(&p["agentType"]) {
        fact.insert("agentType".into(), kind.into());
    }
    fact.insert("childSessionId".into(), child.into());
    if let Some(parent) = text(&p["parentToolCallId"]) {
        fact.insert("parentToolCallId".into(), parent.into());
    }
    fact.insert("background".into(), (p["background"] == true).into());
    if let Some(status) = text(&p["status"]) {
        fact.insert("status".into(), status.into());
    }
    if !spawned && let Some(error) = text(&p["error"]) {
        fact.insert("errorMessage".into(), error.into());
    }
    Some(fact)
}
