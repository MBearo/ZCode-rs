use crate::contract::{ModelFailure, RetryState, ToolOutput};
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
        change: crate::domain::file_checkpoint::FileCheckpoint,
        committed: oneshot::Sender<()>,
    },
    Subagent {
        name: String,
        args: Value,
        call_id: String,
        profile: Option<Box<crate::domain::subagent::Profile>>,
        selection: Option<crate::contract::ModelIdentity>,
        reply: oneshot::Sender<std::result::Result<crate::contract::ChildHandle, String>>,
    },
    GoalStep {
        reply: oneshot::Sender<Option<crate::domain::goal::Goal>>,
    },
    GoalVerdict {
        target_id: String,
        verdict: crate::domain::goal::Verdict,
        usage: Value,
        reply: oneshot::Sender<Option<(crate::domain::goal::Goal, Value)>>,
    },
    SkillsInitialized {
        catalog: crate::domain::skills::SkillCatalog,
        reply: oneshot::Sender<crate::domain::skills::SkillCatalog>,
    },
    Todos {
        call_id: String,
        write: Option<Vec<crate::domain::todo::TodoItem>>,
        reply: oneshot::Sender<ToolOutput>,
    },
    TodoReminder {
        reply: oneshot::Sender<Value>,
    },
    ToolCleanupFailed(String),
    PromptInitialized {
        snapshot: Box<crate::domain::prompt::PromptSnapshot>,
        skills: crate::domain::skills::SkillCatalog,
        committed: oneshot::Sender<crate::domain::skills::SkillCatalog>,
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
        context: crate::domain::context::ContextState,
        tokens: usize,
        usage: Value,
        committed: oneshot::Sender<()>,
    },
    Background {
        task: crate::domain::background::BackgroundTask,
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
        reply: oneshot::Sender<bool>,
    },
    Question {
        call_id: String,
        input: Box<crate::domain::question::QuestionInput>,
        reply: oneshot::Sender<crate::domain::question::QuestionAnswer>,
    },
    ToolDone {
        id: String,
        result: String,
        display: Option<Value>,
        failed: bool,
        committed: oneshot::Sender<()>,
    },
    StepBoundary {
        committed: oneshot::Sender<Option<Vec<Value>>>,
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
#[derive(Clone)]
pub struct EventSink {
    pub session_id: String,
    pub run_id: String,
    pub tx: mpsc::Sender<RunEvent>,
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
