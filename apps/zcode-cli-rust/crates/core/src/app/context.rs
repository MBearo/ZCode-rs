use crate::{
    contract::{ContextPort, Event, EventSink, ModelOutput, ModelPort},
    domain::context::{ContextState, estimate, with_summary},
};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub(super) struct RunContext {
    pub agent_profile: Option<crate::domain::subagent::Profile>,
    pub goal: Option<crate::domain::goal::Goal>,
    pub skills: Option<crate::domain::skills::SkillCatalog>,
    pub prompt_snapshot: Option<crate::domain::prompt::PromptSnapshot>,
    /// A resumed session's stored `envInfo` (Node `extractPersistedEnvInfo`).
    pub stored_env: Option<Value>,
    /// Tools hidden from the provider for this turn (Node turn `toolDisallowlist`).
    pub tool_disallowlist: Vec<String>,
    /// `@plugin` references of the turn's user input (spec rust-m10-plugins §3.9).
    pub plugin_references: Vec<String>,
    /// Tools registered for the session (legacy `toolAllowlist` / `toolDenylist`).
    pub tool_filter: crate::domain::session_runtime::ToolFilter,
    /// Engine-published permission inputs; `None` in tests that bypass the engine.
    pub permissions:
        Option<tokio::sync::watch::Receiver<std::sync::Arc<super::permissions::Snapshot>>>,
    pub state: ContextState,
    pub messages: Vec<Value>,
    pub manual: Option<String>,
    pub usage_anchor: Option<(usize, usize, usize)>,
    /// The pending plan exit reminder was already added in this run.
    pub plan_exit_sent: bool,
    /// Node `modelAnomalyGuard` for this run's tool call reminders.
    pub anomaly_guard: crate::domain::model_anomaly::Guard,
    /// Node `autoCompactConsecutiveFailures` when the run started, kept current by the run.
    pub compact_failures: u32,
    /// Not the session's first turn (Node `turnNumber > 0`).
    pub returning: bool,
    pub(super) estimated: usize,
    /// Messages shown to the model but never persisted, before `messages[position]`.
    pub(super) transient: Vec<Transient>,
}

/// A request-only message (Node in-memory history entry without persistence).
#[derive(Clone)]
pub(super) struct Transient {
    pub position: usize,
    pub kind: TransientKind,
    pub message: Value,
    tokens: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TransientKind {
    Continue,
    /// Kept by the engine across runs of this process.
    Reminder(crate::domain::session_runtime::ReminderKind),
}
const CONTINUE_PROMPT: &str = "Output token limit hit. Resume directly — no apology, no recap of what you were doing. Pick up mid-thought if that is where the cut happened. Break remaining work into smaller pieces.";
fn continuation_tokens() -> usize {
    CONTINUE_PROMPT.encode_utf16().count().div_ceil(3)
}
impl RunContext {
    pub fn new(
        state: ContextState,
        messages: Vec<Value>,
        manual: Option<String>,
        estimated: usize,
    ) -> Self {
        Self {
            agent_profile: None,
            goal: None,
            skills: None,
            prompt_snapshot: None,
            stored_env: None,
            tool_disallowlist: vec![],
            plugin_references: vec![],
            tool_filter: Default::default(),
            permissions: None,
            state,
            messages,
            manual,
            usage_anchor: None,
            plan_exit_sent: false,
            anomaly_guard: Default::default(),
            compact_failures: 0,
            returning: false,
            estimated,
            transient: vec![],
        }
    }
    pub fn push(&mut self, message: Value) {
        self.estimated += estimate(std::slice::from_ref(&message));
        self.messages.push(message);
    }
    pub fn continue_output(&mut self) {
        let message = json!({"role":"user","content":CONTINUE_PROMPT});
        self.add_transient(TransientKind::Continue, message, continuation_tokens());
    }
    /// Adds a request-only message after the current history; returns its position.
    pub fn add_transient(&mut self, kind: TransientKind, message: Value, tokens: usize) -> usize {
        let position = self.messages.len();
        self.transient.push(Transient {
            position,
            kind,
            message,
            tokens,
        });
        position
    }
    /// A reminder placed before `messages[position]` (hook context ahead of the
    /// turn's input), keeping the list ordered by position.
    pub fn insert_transient(&mut self, position: usize, kind: TransientKind, message: Value) {
        let tokens = estimate(std::slice::from_ref(&message));
        let at = self.transient.partition_point(|t| t.position <= position);
        self.transient.insert(
            at,
            Transient {
                position,
                kind,
                message,
                tokens,
            },
        );
    }
    /// Restores reminders the engine kept for this session (positions in `messages`).
    pub fn restore_transient(
        &mut self,
        kept: impl IntoIterator<Item = (usize, TransientKind, Value)>,
    ) {
        for (position, kind, message) in kept {
            let tokens = estimate(std::slice::from_ref(&message));
            self.transient.push(Transient {
                position,
                kind,
                message,
                tokens,
            });
        }
        self.transient.sort_by_key(|t| t.position);
    }
    pub fn transient(&self) -> &[Transient] {
        &self.transient
    }
    fn transient_tokens(&self) -> usize {
        self.transient.iter().map(|t| t.tokens).sum()
    }
    pub fn anchor_usage(&mut self, usage: &Value) {
        self.usage_anchor = usage["prompt_tokens"].as_u64().map(|tokens| {
            (
                tokens as usize,
                self.messages.len(),
                self.transient_tokens(),
            )
        });
    }
    /// Whether automatic compaction has a summarizable part (Node `hasEnoughMessagesToCompact`).
    pub fn can_compact(&self) -> bool {
        crate::domain::compact::select(&self.messages, self.state.summary.is_some(), false)
            .is_some()
    }
    pub fn projection(&self, prefix: &[Value], tool_tokens: usize) -> (Vec<Value>, usize) {
        let mut tokens = self.estimated + estimate(prefix) + tool_tokens + self.transient_tokens();
        // 常规请求保持批量 clone 路径；仅存在临时消息（续写提示、plan 提醒）时逐条合并。
        let mut messages = if self.transient.is_empty() {
            with_summary(self.state.summary.as_deref(), &self.messages)
        } else {
            let mut messages = with_summary(self.state.summary.as_deref(), &[]);
            messages.reserve(self.messages.len() + self.transient.len() + 1);
            let mut transient = self.transient.iter().peekable();
            for index in 0..=self.messages.len() {
                while let Some(entry) = transient.next_if(|t| t.position == index) {
                    messages.push(entry.message.clone());
                }
                if let Some(message) = self.messages.get(index) {
                    messages.push(message.clone());
                }
            }
            messages
        };
        messages.splice(0..0, prefix.iter().cloned());
        if let Some((anchor, count, transient)) = self.usage_anchor {
            tokens = tokens.max(
                anchor
                    .saturating_add(estimate(&self.messages[count..]))
                    .saturating_add(self.transient_tokens().saturating_sub(transient)),
            );
        }
        (messages, tokens)
    }
}
/// A request of the run outside the agent step: retries, auth and network
/// status go to the run, the output does not.
pub(super) async fn hidden_request(
    model: &dyn ModelPort,
    (messages, tools): (Vec<Value>, &[Value]),
    sink: &EventSink,
    query_source: &'static str,
    cancel: &CancellationToken,
) -> Result<ModelOutput> {
    let (tx, mut rx) = mpsc::channel(32);
    let hidden = EventSink {
        session_id: sink.session_id.clone(),
        run_id: sink.run_id.clone(),
        tx,
        // 压缩与工具内部请求不是 agent step，与 Node 一样按 other 归属。
        origin: sink.origin.other(query_source),
        request_auth: sink.request_auth.clone(),
    };
    let request = model.complete(messages, tools, &hidden, cancel);
    tokio::pin!(request);
    let result = loop {
        tokio::select! {biased;
            _=cancel.cancelled()=>bail!("Cancelled"),
            Some(event)=rx.recv()=> {
                if matches!(event.event, Event::RequestAuth{..} | Event::ModelStatus(_)) { sink.send(event.event).await?; }
            },
            result=&mut request=> break result,
        }
    };
    // 请求结束时通道里可能还有未转发的状态（含带 usage 的 completed）。原先直接返回会丢掉它，
    // 压缩与目标验证的用量因此在 run 结束时被记成 error、token 为 0；这里先转发完再返回。
    while let Ok(event) = rx.try_recv() {
        if matches!(
            event.event,
            Event::RequestAuth { .. } | Event::ModelStatus(_)
        ) {
            sink.send(event.event).await?;
        }
    }
    Ok(result?)
}
/// The request prefix of an agent step (system prompt, instructions, skills,
/// profile and goal state), shared by the compaction summary request.
pub(super) async fn step_prefix(
    model: &dyn ModelPort,
    context: &dyn ContextPort,
    history: &RunContext,
    (skills, profile): (
        &crate::domain::skills::SkillCatalog,
        Option<&crate::domain::subagent::Profile>,
    ),
    cancel: &CancellationToken,
) -> Result<Vec<Value>> {
    let instructions = if profile.is_some_and(|p| p.inject_agents_md == Some(false)) {
        vec![]
    } else {
        context.instructions(cancel).await?
    };
    let identity = model.identity();
    let snapshot = history
        .prompt_snapshot
        .as_ref()
        .context("Prompt snapshot missing")?;
    let mut prefix = crate::domain::prompt::prefix(
        snapshot,
        &instructions,
        identity
            .as_ref()
            .map(|id| (id.provider_id.as_str(), id.model_id.as_str())),
        context.desktop(),
    );
    if let Some(reminder) = skills.reminder() {
        prefix.push(reminder);
    }
    if let Some(profile) = profile {
        prefix.push(json!({"role":"system","content":profile.system_prompt}));
    }
    if let Some(goal) = history.goal.as_ref().filter(|g| g.active()) {
        prefix.push(json!({"role":"user","content":format!("<system-reminder>\n{}\n</system-reminder>",goal.prompt("goalState", None))}));
    }
    Ok(prefix)
}
