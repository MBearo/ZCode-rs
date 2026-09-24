//! Node `InMemoryHookRunner.run`: which hooks take part, the order they run
//! in, lifecycle payloads and the merged result. IO comes from a [`Driver`].
use super::output::{self, Callback, Failure, RunResult};
use super::{HookEvent, Registration, display, matches};
use serde_json::{Map, Value};

/// M2.6 admission of one project hook, decided again before every dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Admission {
    pub allowed: bool,
    pub reason_code: Option<String>,
    /// Disabled in config: the hook emits nothing and is not counted.
    pub skip_lifecycle: bool,
}

impl Admission {
    pub const ALLOWED: Self = Self {
        allowed: true,
        reason_code: None,
        skip_lifecycle: false,
    };
}

/// Session event types of the hook lifecycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Started,
    Completed,
    Blocked,
    Failed,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "hook_run_started",
            Self::Completed => "hook_run_completed",
            Self::Blocked => "hook_run_blocked",
            Self::Failed => "hook_run_failed",
        }
    }
}

/// One lifecycle event with Node's payload (`runner.ts` `emitHookEvent`).
#[derive(Clone, Debug, PartialEq)]
pub struct Lifecycle {
    pub kind: Kind,
    pub payload: Value,
}

/// A running hook's identity, enough to report its end later.
#[derive(Clone, Debug)]
pub struct Dispatch {
    pub hook: Registration,
    pub input: Value,
    pub descriptor: Value,
    pub invocation_id: String,
    pub run_id: String,
    pub hook_index: usize,
    pub hook_count: usize,
    pub started_at: u64,
}

impl Dispatch {
    fn lifecycle(&self, kind: Kind, extra: Vec<(&str, Value)>) -> Lifecycle {
        let input = &self.input;
        let mut payload = Map::new();
        let mut put = |key: &str, value: Option<Value>| {
            if let Some(value) = value.filter(|v| !v.is_null()) {
                payload.insert(key.into(), value);
            }
        };
        put("agentName", input.get("agentName").cloned());
        put("descriptor", Some(self.descriptor.clone()));
        put("hookEventName", input.get("hookEventName").cloned());
        put("hookIndex", Some(self.hook_index.into()));
        put("hookCount", Some(self.hook_count.into()));
        put("hookInvocationId", Some(self.invocation_id.clone().into()));
        put("hookRunId", Some(self.run_id.clone().into()));
        put("hookSource", Some(self.hook.source.clone().into()));
        put("matcher", self.hook.matcher.clone().map(Value::from));
        put("requestId", input.get("requestId").cloned());
        put("startedAt", Some(self.started_at.into()));
        put("toolCallId", input.get("toolCallId").cloned());
        put("toolName", input.get("toolName").cloned());
        for (key, value) in extra {
            put(key, Some(value));
        }
        Lifecycle {
            kind,
            payload: Value::Object(payload),
        }
    }
    pub fn started(&self) -> Lifecycle {
        self.lifecycle(Kind::Started, vec![])
    }
    /// A background hook that finished: its output is never merged.
    pub fn completed(&self, now: u64) -> Lifecycle {
        let duration = now.saturating_sub(self.started_at);
        self.lifecycle(
            Kind::Completed,
            vec![
                ("durationMs", duration.into()),
                ("outcome", "success".into()),
            ],
        )
    }
    pub fn failed(&self, now: u64, failure: &Failure) -> Lifecycle {
        let message = display::sanitize(&failure.message);
        self.lifecycle(
            Kind::Failed,
            vec![
                ("durationMs", now.saturating_sub(self.started_at).into()),
                (
                    "errorCode",
                    failure.code.map(Value::from).unwrap_or(Value::Null),
                ),
                ("errorMessage", message.clone().into()),
                ("outcome", failure.outcome.into()),
                ("stderrPreview", message.into()),
            ],
        )
    }
    fn finished(&self, now: u64, callback: &Callback, processed: &RunResult) -> Lifecycle {
        let mut extra = vec![("durationMs", now.saturating_sub(self.started_at).into())];
        let Some(reason) = processed.block_reason() else {
            extra.push(("outcome", "success".into()));
            return self.lifecycle(Kind::Completed, extra);
        };
        extra.push(("outcome", "blocked".into()));
        let reason = display::sanitize(&reason);
        if !reason.is_empty() {
            extra.push(("blockReason", reason.into()));
        }
        let d = &callback.diagnostics;
        let previews = [
            (
                "errorMessage",
                display::diagnostic(d.error_message.as_deref()),
            ),
            (
                "stderrPreview",
                display::diagnostic(d.stderr_preview.as_deref()),
            ),
            (
                "stdoutPreview",
                display::diagnostic(d.stdout_preview.as_deref()),
            ),
        ];
        for (key, value) in previews {
            if let Some(value) = value {
                extra.push((key, value.into()));
            }
        }
        self.lifecycle(Kind::Blocked, extra)
    }
    /// Denied by workspace-trust admission before it could start.
    fn refused(&self, reason_code: Option<&str>) -> Lifecycle {
        let mut extra = vec![("durationMs", 0.into())];
        extra.push((
            "errorCode",
            reason_code.map(Value::from).unwrap_or(Value::Null),
        ));
        extra.push(("outcome", "blocked".into()));
        if let Some(code) = reason_code {
            extra.push(("blockReason", display::sanitize(code).into()));
        }
        self.lifecycle(Kind::Blocked, extra)
    }
}

/// The IO a run needs; the core crate implements it for one run task.
#[allow(async_fn_in_trait)]
pub trait Driver {
    fn admission(&mut self, hook: &Registration) -> Admission;
    fn now(&self) -> u64;
    fn id(&mut self) -> String;
    async fn emit(&mut self, event: Lifecycle);
    /// Runs a foreground hook to its end (timeouts and cancellation included).
    async fn execute(&mut self, dispatch: &Dispatch) -> Result<Callback, Failure>;
    /// Starts an `async` hook; it reports its own end.
    fn spawn(&mut self, dispatch: Dispatch);
}

/// Runs every matching hook of `input`'s event in registration order; a block
/// never stops the later hooks (Node has no short-circuit).
pub async fn run<D: Driver>(
    driver: &mut D,
    hooks: &[Registration],
    input: &Value,
    values: &[String],
) -> RunResult {
    let mut result = RunResult::default();
    let Some(event) = input["hookEventName"].as_str().and_then(HookEvent::parse) else {
        return result;
    };
    let invocation_id = driver.id();
    // skipLifecycle 的 hook 不发事件，也不计入 hookCount；是否放行在每次派发前重新判定。
    let participants: Vec<&Registration> = hooks
        .iter()
        .filter(|h| h.event == event && matches(h.matcher.as_deref(), values))
        .filter(|h| !driver.admission(h).skip_lifecycle)
        .collect();
    let hook_count = participants.len();
    for (hook_index, hook) in participants.into_iter().enumerate() {
        let dispatch = Dispatch {
            hook: hook.clone(),
            input: input.clone(),
            descriptor: display::descriptor(hook, input),
            invocation_id: invocation_id.clone(),
            run_id: driver.id(),
            hook_index,
            hook_count,
            started_at: driver.now(),
        };
        let admission = driver.admission(hook);
        if !admission.allowed {
            let event = dispatch.refused(admission.reason_code.as_deref());
            driver.emit(event).await;
            continue;
        }
        driver.emit(dispatch.started()).await;
        if hook.background() {
            driver.spawn(dispatch);
            continue;
        }
        let executed = driver.execute(&dispatch).await.and_then(|callback| {
            let processed = output::process(event, callback.output.as_ref())?;
            Ok((callback, processed))
        });
        let now = driver.now();
        let lifecycle = match executed {
            Ok((callback, processed)) => {
                let lifecycle = dispatch.finished(now, &callback, &processed);
                output::merge(&mut result, processed);
                lifecycle
            }
            Err(failure) => dispatch.failed(now, &failure),
        };
        driver.emit(lifecycle).await;
    }
    result
}
