use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAttachment {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub media_type: String,
    /// The content bytes (decoded when `data_url`).
    pub total_bytes: u64,
    /// `path` is a Node data URL artifact (`data:<mime>;base64,...`).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub data_url: bool,
    /// The Node resolution of a prompt attachment (spec rust-m11-node-storage §5.3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub node: Option<super::node_journal::files::NodeFile>,
    /// What this run's requests send instead (Node sends a prepared uploaded
    /// image live while its artifact keeps the original).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prepared: Option<Box<StoredAttachment>>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    /// The Node records of this session still to be written, and the Node ids
    /// of its current turn (spec rust-m11-node-storage §5.1).
    #[serde(default)]
    pub node: super::node_journal::NodeJournal,
    #[serde(default)]
    pub file_checkpoints: Vec<super::file_checkpoint::FileCheckpoint>,
    /// Stored Node checkpoints of a resumed session, turned into
    /// `file_checkpoints` once the tools hold their contents.
    #[serde(skip)]
    pub imported_checkpoints: Vec<super::file_checkpoint::ImportedCheckpoint>,
    /// A resumed Node session's context use (the model's window is added by
    /// the engine, as Node's cold usage seed).
    #[serde(skip)]
    pub cold_context_used: Option<u64>,
    /// The main-turn requests' cache use by model message (spec
    /// rust-m9-usage-logs §4.2); rebuilt from the stored branch on load.
    #[serde(skip)]
    pub cache_hits: crate::usage::CacheHits,
    #[serde(default)]
    pub rewind_committed: Option<String>,
    #[serde(default, skip_serializing)]
    pub history: super::history::History,
    #[serde(default)]
    pub row_highwater: u64,
    #[serde(skip)]
    pub history_rewrite: bool,
    #[serde(default)]
    pub agent_profile: Option<super::subagent::Profile>,
    #[serde(default)]
    pub children: std::collections::BTreeMap<String, super::subagent::Task>,
    #[serde(default)]
    pub mailbox: Vec<Value>,
    #[serde(default)]
    pub goal: Option<super::goal::Goal>,
    #[serde(default)]
    pub skills: Option<super::skills::SkillCatalog>,
    #[serde(default)]
    pub shared_context: Option<super::shared_context::SharedContext>,
    #[serde(default)]
    pub legacy_shared_context: bool,
    #[serde(default)]
    pub workspace_path: Option<String>,
    #[serde(default)]
    pub workspace_directory: Option<String>,
    #[serde(default)]
    pub trace_id: Option<String>,
    /// Root trace of this process's runtime for the session (Node creates one
    /// per app instance and never persists it); used for model attribution.
    #[serde(skip)]
    pub runtime_trace: Option<String>,
    #[serde(default)]
    pub todos: Vec<super::todo::TodoItem>,
    #[serde(default)]
    pub todos_updated_at: u64,
    #[serde(default)]
    pub prompt_snapshot: Option<super::prompt::PromptSnapshot>,
    /// The stored `envInfo` of a resumed session (Node `extractPersistedEnvInfo`),
    /// the environment of its next prompt snapshot.
    #[serde(skip)]
    pub stored_env: Option<Value>,
    #[serde(default)]
    pub context: super::context::ContextState,
    #[serde(skip)]
    pub context_tokens: Option<usize>,
    #[serde(skip)]
    pub compact_instructions: Option<String>,
    #[serde(skip)]
    pub queued_now: Option<String>,
    #[serde(skip)]
    pub pending_acks: std::collections::BTreeMap<String, Value>,
    /// Missing in legacy native sessions: those deserialize as build.
    #[serde(default)]
    pub mode: super::execution::Mode,
    #[serde(default)]
    pub plan_enabled: bool,
    /// Last tool-driven plan transition `{toolCallId, planEnabled}` (V4 `config.planTransition`).
    #[serde(default)]
    pub plan_transition: Option<Value>,
    /// Interaction whose full-access approval switched this session to yolo.
    #[serde(default)]
    pub permission_grant: Option<String>,
    /// Mode recorded with the last assistant message (Node `info.mode`); a legacy
    /// cold `session/resume` restores it.
    #[serde(default)]
    pub last_assistant_mode: Option<String>,
    #[serde(skip)]
    pub runtime: super::session_runtime::RuntimeOptions,
    /// Plan was just turned off; the next model request says so once (memory only).
    #[serde(skip)]
    pub needs_plan_exit_reminder: bool,
    #[serde(default)]
    pub parent_id: Option<String>,
    #[serde(default = "interactive")]
    pub task_type: String,
    #[serde(default)]
    pub archived_at: Option<u64>,
    #[serde(default)]
    pub archived: bool,
    #[serde(default = "yes")]
    pub listed: bool,
    #[serde(default)]
    pub attachments: std::collections::BTreeMap<String, StoredAttachment>,
    #[serde(default)]
    pub background: std::collections::BTreeMap<String, super::background::BackgroundTask>,
    pub id: String,
    pub workspace: String,
    pub title: String,
    pub title_source: String,
    pub provider: String,
    pub model: String,
    pub reasoning_level: String,
    #[serde(default)]
    pub thought_levels: Vec<String>,
    pub epoch: String,
    pub seq: u64,
    pub revision: u64,
    pub created_at: u64,
    pub updated_at: u64,
    pub phase: super::execution::Phase,
    #[serde(default, skip_serializing)]
    pub rows: Vec<Value>,
    #[serde(default, skip_serializing)]
    pub messages: Vec<Value>,
    #[serde(skip)]
    pub saved_rows: usize,
    #[serde(skip)]
    pub resident_bytes: Option<usize>,
    /// Retained deltas and the last published patch of the conversation topic.
    #[serde(skip)]
    pub topic: super::topic_log::ConversationTopic,
    #[serde(skip)]
    pub saved_inputs: usize,
    #[serde(skip)]
    pub saved_responses: usize,
    #[serde(skip)]
    pub saved_messages: usize,
    #[serde(skip)]
    pub checkpoint_at: u64,
    #[serde(skip)]
    pub api_retry: Option<super::model::RetryState>,
    pub usage: Value,
    pub last_error: Option<Value>,
    #[serde(default)]
    pub creation_ack: Option<(String, Value)>,
    #[serde(default)]
    pub pending: Vec<Value>,
    #[serde(skip)]
    pub queue: Vec<Value>,
    #[serde(default = "yes")]
    pub auto_drain: bool,
    #[serde(default = "queue_mode")]
    pub followup_mode: String,
    #[serde(skip)]
    pub run_id: Option<String>,
}
fn queue_mode() -> String {
    "queue".into()
}
fn interactive() -> String {
    "interactive".into()
}
fn yes() -> bool {
    true
}

impl Session {
    pub fn new(
        id: String,
        workspace: String,
        provider: String,
        model: String,
        reasoning_level: String,
        epoch: String,
        now: u64,
    ) -> Self {
        Self {
            node: Default::default(),
            file_checkpoints: vec![],
            imported_checkpoints: vec![],
            cold_context_used: None,
            cache_hits: Default::default(),
            rewind_committed: None,
            history: Default::default(),
            row_highwater: 0,
            history_rewrite: false,
            agent_profile: None,
            children: Default::default(),
            mailbox: vec![],
            goal: None,
            skills: None,
            shared_context: None,
            legacy_shared_context: false,
            workspace_path: None,
            workspace_directory: None,
            trace_id: None,
            runtime_trace: None,
            todos: vec![],
            todos_updated_at: now,
            prompt_snapshot: None,
            stored_env: None,
            context: Default::default(),
            context_tokens: None,
            compact_instructions: None,
            queued_now: None,
            pending_acks: Default::default(),
            mode: super::execution::Mode::Build,
            plan_enabled: false,
            plan_transition: None,
            permission_grant: None,
            needs_plan_exit_reminder: false,
            last_assistant_mode: None,
            runtime: Default::default(),
            parent_id: None,
            task_type: interactive(),
            archived_at: None,
            archived: false,
            listed: true,
            attachments: Default::default(),
            background: Default::default(),
            id,
            workspace,
            provider,
            model,
            reasoning_level,
            thought_levels: vec![],
            epoch,
            title: String::new(),
            title_source: "default".into(),
            seq: 0,
            revision: 0,
            created_at: now,
            updated_at: now,
            phase: super::execution::Phase::Draft,
            rows: vec![],
            messages: vec![],
            saved_rows: 0,
            resident_bytes: None,
            topic: Default::default(),
            saved_inputs: 0,
            saved_responses: 0,
            saved_messages: 0,
            checkpoint_at: 0,
            api_retry: None,
            usage: json!({"contextWindow":null,"cumulative":{"inputTokens":0,"outputTokens":0,"cacheReadTokens":0,"cacheWriteTokens":0}}),
            last_error: None,
            creation_ack: None,
            pending: vec![],
            queue: vec![],
            auto_drain: true,
            followup_mode: queue_mode(),
            run_id: None,
        }
    }
    pub fn active_context_tokens(&mut self) -> usize {
        *self.context_tokens.get_or_insert_with(|| {
            super::context::estimate(&self.messages[self.context.offset..])
                + super::context::estimate(&super::context::with_summary(
                    self.context.summary.as_deref(),
                    &[],
                ))
        })
    }
    pub fn append_message(&mut self, message: Value) {
        // canonical 消息只有 actor 追加；估算随追加增量更新，避免长历史每轮重复扫描。
        if let Some(tokens) = &mut self.context_tokens {
            *tokens += super::context::estimate(std::slice::from_ref(&message));
        }
        if message["role"] == "assistant" {
            self.last_assistant_mode = Some(self.mode.as_str().into());
        }
        self.runtime.fresh = false;
        self.messages.push(message);
    }
    pub fn ended(&self) -> bool {
        self.phase.ended()
    }
    pub fn running(&self) -> bool {
        self.run_id.is_some()
    }
    pub fn current_rows_start(&self) -> usize {
        self.rows
            .iter()
            .rposition(|row| row["kind"] == "turnHeader")
            .unwrap_or(0)
    }
    pub fn row(&mut self, kind: &str, turn: &str, entity: &str, now: u64) -> Value {
        self.row_highwater = self.row_highwater.max(
            self.rows
                .last()
                .and_then(|r| r["rowId"].as_u64())
                .unwrap_or(0),
        ) + 1;
        json!({"rowId":self.row_highwater,
            "turnId":turn,"productTurnId":turn,"entityId":entity,"kind":kind,"createdAt":now,"createdAtSeq":self.seq+1})
    }
    pub fn recover(&mut self, epoch: String, now: u64) {
        for task in self.children.values_mut() {
            if task.running() {
                task.status = "lost".into();
                task.ended_at = Some(now);
                task.notified = true;
                task.output =
                    "Child execution was interrupted by runtime exit; it was not replayed.".into();
            }
        }
        if let Some(context) = &mut self.shared_context {
            context.release(None);
        }
        self.epoch = epoch;
        self.seq = 0;
        self.run_id = None;
        self.api_retry = None;
        self.pending.clear();
        self.queue.clear();
        for task in self.background.values_mut() {
            if task.status == "running" {
                task.status = "interrupted".into();
                task.ended_at = Some(now);
            }
        }
        if matches!(
            self.phase,
            super::execution::Phase::Running | super::execution::Phase::Prewarming
        ) {
            self.phase = super::execution::Phase::CompletedInterrupted;
            self.auto_drain = false;
            self.finish_rows("interrupted", now);
            // 崩溃可能发生于 assistant tool_calls 已保存但 tool result 尚未返回；
            // 补齐取消结果只用于模型语法恢复，绝不能重新执行有副作用的工具。
            self.close_unfinished_tools();
        }
        self.recover_subagent_rows();
        // Node onSessionResumed：重启后不可能仍在运行的 hook 行收口为失败（执行项为 cancelled）。
        for row in &mut self.rows {
            crate::hooks::projection::close_running(row, now);
        }
    }
}
