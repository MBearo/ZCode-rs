use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredAttachment {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub media_type: String,
    pub total_bytes: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    #[serde(default)]
    pub file_checkpoints: Vec<super::file_checkpoint::FileCheckpoint>,
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
    #[serde(default)]
    pub todos: Vec<super::todo::TodoItem>,
    #[serde(default)]
    pub todos_updated_at: u64,
    #[serde(default)]
    pub prompt_snapshot: Option<super::prompt::PromptSnapshot>,
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
    #[serde(default = "legacy_mode")]
    pub mode: String,
    #[serde(default)]
    pub plan_enabled: bool,
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
    pub phase: String,
    #[serde(default, skip_serializing)]
    pub rows: Vec<Value>,
    #[serde(default, skip_serializing)]
    pub messages: Vec<Value>,
    #[serde(skip)]
    pub saved_rows: usize,
    #[serde(skip)]
    pub resident_bytes: Option<usize>,
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
fn legacy_mode() -> String {
    "build".into()
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
            file_checkpoints: vec![],
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
            todos: vec![],
            todos_updated_at: now,
            prompt_snapshot: None,
            context: Default::default(),
            context_tokens: None,
            compact_instructions: None,
            queued_now: None,
            pending_acks: Default::default(),
            mode: "yolo".into(),
            plan_enabled: false,
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
            phase: "draft".into(),
            rows: vec![],
            messages: vec![],
            saved_rows: 0,
            resident_bytes: None,
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
        self.messages.push(message);
    }
    pub fn ended(&self) -> bool {
        self.phase.starts_with("completed")
    }
    fn projected_title_source(&self) -> &str {
        // TS stored 身份有 first_input，V4 只有三值；与 product-projection 统一映射，避免冷恢复帧被拒绝。
        if self.title_source == "first_input" {
            "generated"
        } else {
            &self.title_source
        }
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
    pub fn patch(&self) -> Value {
        let mut patch = json!({"revision":self.revision,
            "control":{"phase":self.phase,"sessionEnded":self.ended(),"canStop":self.running(),
                "stopState":if self.running(){"stoppable"}else{"idle"},"stopTargetKind":if self.running(){"mixed"}else{"unknown"},
                "activeWorks":self.run_id.as_ref().map(|id| vec![json!({"kind":"primaryTurn","foregroundExecutionId":id,"startedAt":self.updated_at})]).unwrap_or_default(),
                "lastError":self.last_error,"apiRetry":self.api_retry},
            "availability":{"fork":if self.history.responses.is_empty(){json!({"allowed":false,"reasonCode":"guard.forkTargetNotStable"})}else{json!({"allowed":true})},"compact":{"allowed":true},"switchModelConfig":{"allowed":true},
                "setFollowupMode":{"allowed":true},"queueEdit":{"allowed":true},"sendQueuedNow":if self.queued_now.is_none(){json!({"allowed":true})}else{json!({"allowed":false,"reasonCode":"guard.queuePromotionBusy"})},
                "pauseGoal":if self.goal.as_ref().is_some_and(|g|g.active()){json!({"allowed":true})}else{json!({"allowed":false,"reasonCode":if self.goal.is_none(){"noGoalToPause"}else{"goalNotActive"}})},
                "resumeGoal":if self.goal.as_ref().is_some_and(|g|matches!(g.status.as_str(),"paused"|"failed"|"notSatisfied")) && !self.running(){json!({"allowed":true})}else{json!({"allowed":false,"reasonCode":if self.goal.is_none(){"noGoalToResume"}else{"goalNotPaused"}})}},
            "inputRouting":{"mode":if self.running(){if self.followup_mode=="guide"{"guide"}else{"enqueue"}}else if !self.auto_drain && !self.queue.is_empty(){"choice"}else{"startNow"}},
            "meta":{"title":self.title,"titleSource":self.projected_title_source()},
            "config":{"provider":self.provider,"model":self.model,"thought":self.reasoning_level,"thoughtLevels":if self.thought_levels.is_empty(){vec![self.reasoning_level.clone()]}else{self.thought_levels.clone()},
                "modelSelection":{"providerId":self.provider,"modelId":self.model,"options":{"reasoningLevel":self.reasoning_level}},"followupMode":self.followup_mode,"mode":self.mode,"planEnabled":self.plan_enabled},
            "usage":self.usage,"queue":{"items":self.queue.iter().filter(|q|q["delivery"]["admitted"]!="startNow").collect::<Vec<_>>(),"autoDrain":self.auto_drain},
            "pendingInteractions":self.pending,"pendingCommands":[],"backgroundWorks":self.background.values().filter(|t|t.status=="running").map(|t|t.projection()).collect::<Vec<_>>(),"goal":self.goal.as_ref().map(|g|g.projection()),"plan":super::todo::plan(&self.todos,self.todos_updated_at)});
        if self.provider.is_empty() || self.model.is_empty() {
            patch["config"]
                .as_object_mut()
                .unwrap()
                .remove("modelSelection");
        }
        if let Some(context) = &self.shared_context {
            patch["sharedContextImport"] = context.projection(&self.title);
        } else if self.legacy_shared_context {
            patch["sharedContextImport"] = json!({"title":self.title});
        }
        super::subagent::projection(self, &mut patch);
        patch
    }
    pub fn snapshot(&self) -> Value {
        let mut value = self.patch();
        let tail = self.rows.len().saturating_sub(60);
        value["protocolVersion"] = 1.into();
        value["sessionId"] = self.id.clone().into();
        value["logEpoch"] = self.epoch.clone().into();
        value["seq"] = self.seq.into();
        value["rows"] = json!({"window":self.rows[tail..],"totalCount":self.rows.len(),"firstRowId":self.rows.first().map(|v|&v["rowId"])});
        value
    }
    pub fn summary(&self) -> Value {
        let mut summary = json!({"sessionId":self.id,"workspaceId":self.workspace,"title":self.title,"titleSource":self.projected_title_source(),
            "phase":self.phase,"sessionEnded":self.ended(),"hasBackgroundWork":self.background.values().any(|t|t.status=="running"),"lastActivityAt":self.updated_at,"createdAt":self.created_at,
            "pendingInteractionSummary":{"permissionCount":self.pending.iter().filter(|p|p["kind"]=="permission").count(),"userInputCount":self.pending.iter().filter(|p|p["kind"]=="userInput").count()}});
        if let Some(p) = self.pending.first() {
            let mut interaction = json!({"interactionId":p["interactionId"],"kind":p["kind"]});
            if let Some(name) = p["payload"].get("toolName") {
                interaction["toolName"] = name.clone();
            }
            if let Some(a) = p.get("autoResolution") {
                interaction["autoResolution"] = a.clone();
            }
            summary["pendingInteraction"] = interaction;
        }
        if let Some(parent) = &self.parent_id {
            summary["parentSessionId"] = parent.clone().into();
        }
        if let Some(goal) = &self.goal {
            summary["goalStatus"] = goal.status.clone().into();
        }
        if self.children.values().any(|t| t.running()) {
            summary["hasBackgroundWork"] = true.into();
        }
        summary
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
        if let Some(goal) = &mut self.goal {
            // 重启只结算最后确认的活跃时间，不把进程离线期间算作模型工作。
            goal.pause(goal.last_seen.unwrap_or(now));
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
        if self.phase == "running" || self.phase == "prewarming" {
            self.phase = "completedInterrupted".into();
            self.auto_drain = false;
            self.finish_rows("interrupted", now);
            // 崩溃可能发生于 assistant tool_calls 已保存但 tool result 尚未返回；
            // 补齐取消结果只用于模型语法恢复，绝不能重新执行有副作用的工具。
            self.close_unfinished_tools();
        }
        self.recover_subagent_rows();
    }
}
