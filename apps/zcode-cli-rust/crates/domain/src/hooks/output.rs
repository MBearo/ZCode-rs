//! Hook results (Node `configured-runner-callback.ts` and `output.ts`): the
//! process result as output, one output's effect, and merging across hooks.
use super::{HookEvent, truncate_utf16};
use crate::permission::Behavior;
use serde_json::{Value, json};

/// The finished process (Node `ExecutionResult`, only what hooks read).
#[derive(Clone, Debug, Default)]
pub struct Exec {
    /// `completed`, `failed`, `timed_out` or `cancelled`.
    pub status: String,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    /// Start or stream failure text.
    pub error: Option<String>,
}

/// Previews kept for a blocked lifecycle event only.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Diagnostics {
    pub error_message: Option<String>,
    pub stderr_preview: Option<String>,
    pub stdout_preview: Option<String>,
}

/// A hook that ran: its validated output (unknown keys removed) if any.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Callback {
    pub output: Option<Value>,
    pub diagnostics: Diagnostics,
}

/// A hook that failed (Node CoreError): lifecycle `errorCode`, `outcome` and
/// the message clients see (the cause when there is one).
#[derive(Clone, Debug, PartialEq)]
pub struct Failure {
    pub code: Option<&'static str>,
    pub outcome: &'static str,
    pub message: String,
}

impl Failure {
    pub fn timeout(timeout_ms: u64) -> Self {
        Self {
            code: Some("TOOL_TIMEOUT"),
            outcome: "timed_out",
            message: format!("Hook timed out after {timeout_ms}ms"),
        }
    }
    pub fn cancelled() -> Self {
        Self {
            code: Some("TOOL_CANCELLED"),
            outcome: "cancelled",
            message: "Hook execution cancelled".into(),
        }
    }
    pub fn configuration(message: String) -> Self {
        Self {
            code: Some("CONFIGURATION_ERROR"),
            outcome: "failed",
            message,
        }
    }
    fn execution(message: String) -> Self {
        Self {
            code: Some("TOOL_EXECUTION_FAILED"),
            outcome: "failed",
            message,
        }
    }
    /// A plain error outside the hook contract (temp file, spawn port).
    pub fn other(message: String) -> Self {
        Self {
            code: None,
            outcome: "failed",
            message,
        }
    }
}

/// Node `trimForPreview`.
fn preview(text: &str) -> Option<String> {
    let trimmed = text.trim();
    (!trimmed.is_empty()).then(|| truncate_utf16(trimmed, 4000, "..."))
}

/// Node `processHookExecutionResult`: exit 0 parses stdout, exit 2 blocks,
/// anything else fails the hook without blocking.
pub fn callback(event: HookEvent, exec: &Exec) -> Result<Callback, Failure> {
    let exit = exec.exit_code;
    let output = if exec.status == "completed" && exit.unwrap_or(0) == 0 {
        parse_stdout(&exec.stdout)?
    } else if exit == Some(2) {
        Some(exit_block(event, exec))
    } else {
        let cause = exec
            .error
            .clone()
            .or_else(|| preview(&exec.stderr))
            .unwrap_or_else(|| format!("Hook process exited with status {}", exec.status));
        return Err(Failure::execution(cause));
    };
    let stderr = preview(&exec.stderr);
    Ok(Callback {
        output,
        diagnostics: Diagnostics {
            error_message: stderr.clone(),
            stderr_preview: stderr,
            stdout_preview: (exit == Some(2)).then(|| preview(&exec.stdout)).flatten(),
        },
    })
}

fn exit_block(event: HookEvent, exec: &Exec) -> Value {
    let reason = preview(&exec.stderr)
        .or_else(|| preview(&exec.stdout))
        .unwrap_or_else(|| "Hook blocked execution".into());
    match event {
        HookEvent::PreToolUse => json!({"continue":false,"reason":reason,"hookSpecificOutput":{
            "hookEventName":"PreToolUse","permissionDecision":"deny","permissionDecisionReason":reason}}),
        HookEvent::PermissionRequest => {
            json!({"continue":false,"reason":reason,"hookSpecificOutput":{
            "hookEventName":"PermissionRequest","decision":{"behavior":"deny","message":reason}}})
        }
        HookEvent::Stop => json!({"decision":"block","reason":reason}),
        _ => json!({"continue":false,"reason":reason}),
    }
}

const SCHEMA_FAILED: &str = "Hook stdout failed HookJSONOutput schema validation";

/// Node `parseHookStdout`: non-JSON text is ignored (a truncated JSON too, D8);
/// JSON that fails `HookJSONOutputSchema` fails the hook.
fn parse_stdout(stdout: &str) -> Result<Option<Value>, Failure> {
    let trimmed = stdout.trim();
    if !trimmed.starts_with('{') {
        return Ok(None);
    }
    let Ok(value) = serde_json::from_str::<Value>(trimmed) else {
        return Ok(None);
    };
    super::schema::validate(&value)
        .map(Some)
        .ok_or_else(|| Failure::execution(SCHEMA_FAILED.into()))
}

/// Node `HookRunResult`.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunResult {
    pub additional_contexts: Vec<String>,
    pub block_requested: bool,
    pub decision_reason: Option<String>,
    pub behavior: Option<Behavior>,
    /// PermissionRequest `decision` (`{behavior, …}`).
    pub request_decision: Option<Value>,
    pub prevent_continuation: bool,
    pub stop_should_continue: bool,
    pub stop_reason: Option<String>,
    /// `Some(Value::Null)` when a hook set `updatedInput: null`.
    pub updated_input: Option<Value>,
}

pub(super) fn text(v: &Value, key: &str) -> Option<String> {
    v[key].as_str().map(str::to_owned)
}
fn truthy(v: &Value, key: &str) -> Option<String> {
    text(v, key).filter(|s| !s.is_empty())
}
fn behavior(name: &str) -> Option<Behavior> {
    match name {
        "allow" => Some(Behavior::Allow),
        "ask" => Some(Behavior::Ask),
        "deny" => Some(Behavior::Deny),
        _ => None,
    }
}

/// Node `processHookOutput`. `Err` when the specific output names another
/// event (`Hook returned wrong event name`), which fails the hook.
pub fn process(event: HookEvent, output: Option<&Value>) -> Result<RunResult, Failure> {
    let mut r = RunResult::default();
    let Some(o) = output else {
        return Ok(r);
    };
    let first = |a: &str, b: &str| text(o, a).or_else(|| text(o, b));
    if o["continue"] == false && event != HookEvent::Stop {
        r.block_requested = true;
        r.stop_reason = first("stopReason", "reason");
        r.prevent_continuation = event.prevents();
        if event.permission() {
            r.behavior = Some(Behavior::Deny);
        }
    }
    if event == HookEvent::Stop && o["continue"] == true {
        r.stop_should_continue = true;
        r.stop_reason = first("stopReason", "reason");
    }
    if o["decision"] == "approve" && event.permission() {
        r.behavior = Some(Behavior::Allow);
    }
    if o["decision"] == "block" {
        r.block_requested = true;
        r.stop_reason = first("stopReason", "reason").or_else(|| text(o, "systemMessage"));
        if event.permission() {
            r.behavior = Some(Behavior::Deny);
        }
        r.prevent_continuation |= event.prevents();
        if event == HookEvent::Stop {
            r.stop_should_continue = true;
            r.additional_contexts.extend(truthy(o, "systemMessage"));
            r.additional_contexts.extend(truthy(o, "reason"));
        }
    }
    r.additional_contexts.extend(truthy(o, "additionalContext"));
    r.additional_contexts
        .extend(truthy(o, "additional_context"));
    let Some(specific) = o.get("hookSpecificOutput") else {
        return Ok(r);
    };
    if specific["hookEventName"] != event.as_str() {
        return Err(Failure::execution("Hook returned wrong event name".into()));
    }
    match event {
        HookEvent::PreToolUse => {
            if let Some(decision) = specific["permissionDecision"].as_str().and_then(behavior) {
                r.behavior = Some(decision);
                r.decision_reason = text(specific, "permissionDecisionReason");
            }
            if let Some(input) = specific.get("updatedInput") {
                r.updated_input = Some(input.clone());
            }
        }
        HookEvent::PermissionRequest => {
            r.request_decision = specific.get("decision").cloned();
            return Ok(r);
        }
        _ => {}
    }
    r.additional_contexts
        .extend(truthy(specific, "additionalContext"));
    Ok(r)
}

/// Node `mergeHookRunResult`, including D9a (the last PermissionRequest
/// decision wins) and D9b (a later block erases an earlier stop reason).
pub fn merge(target: &mut RunResult, next: RunResult) {
    target.additional_contexts.extend(next.additional_contexts);
    if next.block_requested {
        target.block_requested = true;
        target.stop_reason = next.stop_reason.clone().or(target.stop_reason.take());
    }
    if next.prevent_continuation {
        target.prevent_continuation = true;
        target.stop_reason = next.stop_reason.clone();
    }
    if next.stop_should_continue {
        target.stop_should_continue = true;
        target.stop_reason = next.stop_reason.clone().or(target.stop_reason.take());
    }
    if next.updated_input.is_some() {
        target.updated_input = next.updated_input;
    }
    if next.request_decision.is_some() {
        target.request_decision = next.request_decision;
    }
    if next
        .decision_reason
        .as_deref()
        .is_some_and(|r| !r.is_empty())
    {
        target.decision_reason = next.decision_reason;
    }
    if let Some(next) = next.behavior {
        target.behavior = Some(match (target.behavior, next) {
            (Some(Behavior::Deny), _) | (_, Behavior::Deny) => Behavior::Deny,
            (Some(Behavior::Ask), _) | (_, Behavior::Ask) => Behavior::Ask,
            _ => next,
        });
    }
}

impl RunResult {
    /// Whether this one hook's result reports as blocked, and why.
    pub fn block_reason(&self) -> Option<String> {
        let denied = self
            .request_decision
            .as_ref()
            .filter(|d| d["behavior"] == "deny");
        let blocked = self.behavior == Some(Behavior::Deny)
            || denied.is_some()
            || self.prevent_continuation
            || self.block_requested;
        blocked.then(|| {
            self.stop_reason
                .clone()
                .or_else(|| self.decision_reason.clone())
                .or_else(|| denied.and_then(|d| text(d, "message")))
                .unwrap_or_else(|| "Hook blocked execution".into())
        })
    }
    /// The Node object shape (fields only when set).
    pub fn to_value(&self) -> Value {
        let mut out = json!({"additionalContexts": self.additional_contexts});
        let mut put = |key: &str, value: Value| {
            out[key] = value;
        };
        if self.block_requested {
            put("blockRequested", true.into());
        }
        if let Some(reason) = &self.decision_reason {
            put("hookPermissionDecisionReason", reason.clone().into());
        }
        if let Some(b) = self.behavior {
            put("permissionBehavior", serde_json::to_value(b).unwrap());
        }
        if let Some(d) = &self.request_decision {
            put("permissionRequestResult", d.clone());
        }
        if self.prevent_continuation {
            put("preventContinuation", true.into());
        }
        if self.stop_should_continue {
            put("stopShouldContinue", true.into());
        }
        if let Some(reason) = &self.stop_reason {
            put("stopReason", reason.clone().into());
        }
        if let Some(input) = &self.updated_input {
            put("updatedInput", input.clone());
        }
        out
    }
}
