use crate::{ModelFailure, RetryState, ToolOutput};
use anyhow::Result;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};

pub struct RunEvent {
    pub session_id: String,
    pub run_id: String,
    pub event: Event,
}
pub enum Event {
    FilePrepared {
        change: zcode_cli_domain::file_checkpoint::FileCheckpoint,
        committed: oneshot::Sender<()>,
    },
    Subagent {
        name: String,
        args: Value,
        call_id: String,
        profile: Option<Box<zcode_cli_domain::subagent::Profile>>,
        selection: Option<crate::ModelIdentity>,
        reply: oneshot::Sender<std::result::Result<crate::ChildHandle, String>>,
    },
    GoalStep {
        reply: oneshot::Sender<Option<zcode_cli_domain::goal::Goal>>,
    },
    GoalVerdict {
        target_id: String,
        verdict: zcode_cli_domain::goal::Verdict,
        usage: Value,
        reply: oneshot::Sender<Option<(zcode_cli_domain::goal::Goal, Value)>>,
    },
    SkillsInitialized {
        catalog: zcode_cli_domain::skills::SkillCatalog,
        reply: oneshot::Sender<zcode_cli_domain::skills::SkillCatalog>,
    },
    Todos {
        call_id: String,
        write: Option<Vec<zcode_cli_domain::todo::TodoItem>>,
        reply: oneshot::Sender<ToolOutput>,
    },
    TodoReminder {
        reply: oneshot::Sender<Value>,
    },
    /// EnterPlanMode / ExitPlanMode switching plan (Node `source: "tool"`); the
    /// error text becomes the tool failure.
    PlanMode {
        call_id: String,
        enable: bool,
        reply: oneshot::Sender<std::result::Result<(), String>>,
    },
    /// A transient plan reminder added before a model request, after `anchor`
    /// session messages; `exit` marks the one-off exit reminder.
    PlanReminder {
        anchor: usize,
        exit: bool,
        message: Value,
    },
    ToolCleanupFailed(String),
    PromptInitialized {
        snapshot: Box<zcode_cli_domain::prompt::PromptSnapshot>,
        skills: zcode_cli_domain::skills::SkillCatalog,
        committed: oneshot::Sender<zcode_cli_domain::skills::SkillCatalog>,
    },
    AuxiliaryDone {
        result: std::result::Result<Value, ModelFailure>,
    },
    RequestAuth {
        provider: String,
        selection: Value,
        access: Value,
        reply: oneshot::Sender<Value>,
    },
    ContextUsage(Value),
    CompactStarted {
        id: String,
        manual: bool,
        tokens: usize,
        committed: oneshot::Sender<()>,
    },
    CompactDone {
        id: String,
        context: zcode_cli_domain::context::ContextState,
        tokens: usize,
        usage: Value,
        /// Plan file reminder appended after the preserved messages.
        reminder: Option<Value>,
        committed: oneshot::Sender<()>,
    },
    Background {
        task: zcode_cli_domain::background::BackgroundTask,
        committed: Option<oneshot::Sender<()>>,
    },
    Retry(Option<RetryState>),
    Text {
        response_id: String,
        text: String,
        reasoning: bool,
    },
    ModelDone {
        stable: bool,
        message: Option<Value>,
        usage: Value,
        committed: oneshot::Sender<()>,
    },
    ToolStart {
        call: Value,
    },
    Permission {
        call: Value,
        request: PermissionRequest,
        reply: oneshot::Sender<PermissionAnswer>,
    },
    Question {
        call_id: String,
        input: Box<zcode_cli_domain::question::QuestionInput>,
        reply: oneshot::Sender<zcode_cli_domain::question::QuestionAnswer>,
    },
    ToolDone {
        id: String,
        result: String,
        display: Option<Value>,
        failed: bool,
        /// Permission denied: the row is cancelled instead of failed (Node `permission_denied`).
        denied: bool,
        committed: oneshot::Sender<()>,
    },
    StepBoundary {
        committed: oneshot::Sender<Option<Guide>>,
    },
    Finished {
        error: Option<String>,
        model_failure: Option<ModelFailure>,
        cancelled: bool,
    },
}
pub struct ModelOutput {
    pub message: Value,
    pub calls: Vec<Value>,
    pub usage: Value,
    pub output_limit: bool,
}
/// A tool call the policy asked the user about.
pub struct PermissionRequest {
    /// Decision reason, shown as the prompt summary.
    pub reason: String,
    /// Arguments as the policy saw them.
    pub input: Value,
    pub suggestions: Vec<zcode_cli_domain::permission::Update>,
    /// Tool `askOptions.allowAlways`: `no-always-allow` / `session-always-allow`.
    pub options_policy: Option<String>,
}
/// The resolved prompt (Node broker result).
#[derive(Debug, PartialEq)]
pub enum PermissionAnswer {
    Allow,
    /// `preserve`: user feedback is kept verbatim instead of summarized.
    Deny {
        message: String,
        preserve: bool,
    },
    /// Allowed, but the tool fails before running (Node: project rule write failed).
    Fail(String),
    /// ExitPlanMode not approved; `Some` carries the user's feedback.
    PlanRejected(Option<String>),
}
/// Messages committed at a step boundary (guided input or subagent mailbox).
pub struct Guide {
    pub messages: Vec<Value>,
    /// The run's new request origin when a guided user input changed it.
    pub origin: Option<std::sync::Arc<RequestOrigin>>,
    /// Tools the guided input hides from the rest of the turn (merged into the run).
    pub tool_disallowlist: Vec<String>,
}
/// `modelExecution.requestAuth` (`{apiKey?, headers?}`) frozen for one execution.
/// It replaces the Host credential request and is never persisted or logged.
pub struct RequestAuth(pub Value);
impl std::fmt::Debug for RequestAuth {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RequestAuth(<redacted>)")
    }
}
/// Model request source, sent as `x-zcode-session-type`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RequestKind {
    Main,
    Subagent,
    #[default]
    Other,
}
impl RequestKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Subagent => "subagent",
            Self::Other => "other",
        }
    }
}
/// Attribution of model requests (Node `ModelStatusContext`). Owned by the
/// engine; a run only holds a copy that the engine replaces on guided input.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestOrigin {
    pub kind: RequestKind,
    pub session_id: Option<String>,
    pub trace_id: String,
    pub query_id: Option<String>,
}
impl RequestOrigin {
    /// A request outside any session run, with a trace of its own.
    pub fn detached(trace_id: String) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            trace_id,
            ..Self::default()
        })
    }
    /// The same run context for work that is not an agent step (compaction).
    pub fn auxiliary(&self) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            kind: RequestKind::Other,
            ..self.clone()
        })
    }
}
#[derive(Clone)]
pub struct EventSink {
    pub session_id: String,
    pub run_id: String,
    pub tx: mpsc::Sender<RunEvent>,
    pub origin: std::sync::Arc<RequestOrigin>,
    /// Credentials frozen for this execution; `None` asks the Host when required.
    pub request_auth: Option<std::sync::Arc<RequestAuth>>,
}
impl EventSink {
    pub async fn send(&self, event: Event) -> Result<()> {
        self.tx
            .send(RunEvent {
                session_id: self.session_id.clone(),
                run_id: self.run_id.clone(),
                event,
            })
            .await?;
        Ok(())
    }
}
