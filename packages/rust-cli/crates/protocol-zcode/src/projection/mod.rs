//! 会话投影（docs/specs/rust-cli/app-protocol.md §6.1）：内核事件 → App 的行与状态。纯计算：`(投影, 事件) → (投影',
//! 增量)`，从 [`SessionView`] 能重建（[`cold`]），所以不是第二份事实。revision 只来自事件（会话 actor 是 owner，G1）。
//!
//! 行：turnHeader（回合）、userInput（真人输入）、assistantText、reasoning、toolCall；其余事件只改状态或忽略（P1）。

mod cold;
mod rows;
pub mod state;

use std::collections::HashMap;

use rust_cli_kernel_api::message::{ContentBlock, ToolResultContent, Usage};
use rust_cli_kernel_api::model::ModelSelection;
use rust_cli_kernel_api::permission::PermissionMode;
use rust_cli_kernel_api::session_api::{
    KernelEvent, QueuedInput, SequencedEvent, SessionView, TurnOutcome,
};
use rust_cli_kernel_api::tool::ToolOutput;
use serde_json::{Map, Value, json};

use state::{ActiveWork, Followup, LastError, Phase};

/// 快照里最多带的行（尾部窗口）。
const WINDOW_ROWS: usize = 200;

/// sendText 带来的输入的事实（行与队列项要用）。
#[derive(Debug, Clone, PartialEq)]
pub struct InputMeta {
    pub client: String,
    pub origin: Option<String>,
    pub text: String,
    pub selection: Option<Value>,
    pub mode: Option<String>,
    pub plan: Option<bool>,
    pub admitted_at: u64,
    pub requested: &'static str,
    pub admitted: &'static str,
}

/// 一个回合里的行。
#[derive(Debug, Clone)]
struct Turn {
    id: String,
    command: Option<String>,
    started_at: u64,
    /// 当前模型响应的序号（流式增量带来；assistant 消息落定时用它找行）。
    response: Option<u32>,
    keys: Vec<String>,
}

/// 一个会话的投影。
#[derive(Debug, Clone)]
pub struct Projection {
    session: String,
    rows: Vec<Map<String, Value>>,
    index: HashMap<String, usize>,
    next_row: u64,
    revision: u64,
    phase: Phase,
    active: Option<ActiveWork>,
    error: Option<LastError>,
    executions: u64,
    title: String,
    selection: ModelSelection,
    mode: PermissionMode,
    plan: bool,
    followup: Followup,
    levels: Vec<String>,
    context: Option<(u64, u64)>,
    total: Usage,
    queue: Vec<QueuedInput>,
    pending: Vec<Value>,
    turn: Option<Turn>,
    inputs: HashMap<String, InputMeta>,
    /// 用户拒绝了的工具调用：结果投成 cancelled。
    denied: std::collections::HashSet<String>,
    created_at: u64,
}

fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|block| match block {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn output_text(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|block| match block {
            ToolResultContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn turn_key(turn: &str) -> String {
    format!("turn:{turn}")
}

fn block_key(kind: &str, turn: &str, response: u32, index: usize) -> String {
    format!("{kind}:{turn}:{response}:{index}")
}

fn tool_key(tool_use: &str) -> String {
    format!("tool:{tool_use}")
}

impl Projection {
    /// 从视图建投影（冷打开或第一次订阅）。`followup` 与 `levels` 是协议自己保存的配置（不在内核里）。
    pub fn from_view(view: &SessionView, followup: Followup, levels: Vec<String>) -> Self {
        let mut projection = Self {
            session: view.meta.id.as_str().to_owned(),
            rows: Vec::new(),
            index: HashMap::new(),
            next_row: 1,
            revision: view.revision,
            phase: Phase::Draft,
            active: None,
            error: None,
            executions: 0,
            title: view.meta.title.clone(),
            selection: view.selection.clone(),
            mode: view.mode,
            plan: view.plan,
            followup,
            levels,
            context: view
                .context_usage
                .as_ref()
                .map(|usage| (usage.used_tokens, usage.max_tokens)),
            total: Usage::default(),
            queue: view.queue.clone(),
            pending: Vec::new(),
            turn: None,
            inputs: HashMap::new(),
            denied: std::collections::HashSet::new(),
            created_at: view.meta.created_at_ms,
        };
        cold::rows(&mut projection, view);
        for (interaction, request) in &view.pending {
            projection.ask(interaction.as_str(), request, view.meta.created_at_ms);
        }
        projection
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn phase(&self) -> Phase {
        self.phase
    }

    pub fn followup(&self) -> Followup {
        self.followup
    }

    pub fn title(&self) -> &str {
        &self.title
    }

    /// 最后一条 assistant 正文（会话列表的预览）。
    pub fn last_reply(&self) -> Option<String> {
        self.rows.iter().rev().find_map(|row| {
            (row["kind"] == "assistantText")
                .then(|| row["text"].as_str().map(str::to_owned))
                .flatten()
        })
    }

    pub fn pending_permissions(&self) -> usize {
        self.pending.len()
    }

    /// 用户拒绝了这个工具调用（resolveInteraction 的裁决在内核事件之前）：结果投成 cancelled（与 Node 相同）。
    pub fn deny(&mut self, tool_use: &str) {
        self.denied.insert(tool_use.to_owned());
    }

    /// 记下 sendText 的事实（行与队列项要用）。
    pub fn remember_input(&mut self, command: &str, meta: InputMeta) {
        self.inputs.insert(command.to_owned(), meta);
    }

    /// 协议自己保存的配置变了（setFollowupMode）：返回 `state.updated`。
    pub fn set_followup(&mut self, followup: Followup) -> Value {
        self.followup = followup;
        json!({"op": "state.updated", "patch": {
            "config": self.config(),
            "inputRouting": state::input_routing(self.phase, self.followup),
        }})
    }

    fn config(&self) -> Value {
        state::config(
            &self.selection,
            self.mode,
            self.plan,
            self.followup,
            &self.levels,
        )
    }

    fn base(
        &self,
        key_turn: &str,
        entity: &str,
        kind: &str,
        now: u64,
        seq: u64,
    ) -> Map<String, Value> {
        let mut row = Map::new();
        row.insert("rowId".into(), json!(self.next_row));
        row.insert("turnId".into(), json!(key_turn));
        row.insert("entityId".into(), json!(entity));
        row.insert("productTurnId".into(), json!(key_turn));
        row.insert("visibility".into(), json!("visible"));
        row.insert("createdAt".into(), json!(now));
        row.insert("createdAtSeq".into(), json!(seq));
        row.insert("kind".into(), json!(kind));
        row
    }

    fn append(&mut self, key: String, row: Map<String, Value>, out: &mut Vec<Value>) {
        self.next_row += 1;
        out.push(json!({"op": "row.appended", "row": row}));
        self.index.insert(key.clone(), self.rows.len());
        self.rows.push(row);
        if let Some(turn) = &mut self.turn {
            turn.keys.push(key);
        }
    }

    fn upsert(
        &mut self,
        key: &str,
        change: impl FnOnce(&mut Map<String, Value>),
        out: &mut Vec<Value>,
    ) {
        let Some(&at) = self.index.get(key) else {
            return;
        };
        let row = &mut self.rows[at];
        let before = row.clone();
        change(row);
        if *row != before {
            out.push(json!({"op": "row.upserted", "row": row.clone()}));
        }
    }

    fn row_id(&self, key: &str) -> Option<u64> {
        self.index
            .get(key)
            .and_then(|at| self.rows[*at]["rowId"].as_u64())
    }

    fn control(&self) -> Value {
        state::control(self.phase, self.active.as_ref(), self.error.as_ref())
    }

    /// 回合的开始、结束等改变阶段的事件：control、availability 与 inputRouting 一起换。
    fn phase_patch(&self, patch: &mut Map<String, Value>) {
        patch.insert("control".into(), self.control());
        patch.insert("availability".into(), state::availability());
        patch.insert(
            "inputRouting".into(),
            state::input_routing(self.phase, self.followup),
        );
    }

    /// 一个事件 → 增量（`row.*` 与 `state.updated`）。`now` 是行的时间。
    pub fn apply(&mut self, event: &SequencedEvent, now: u64) -> Vec<Value> {
        let mut out = Vec::new();
        let mut patch = Map::new();
        let seq = event.seq;
        match &event.event {
            KernelEvent::TurnStarted { turn, command } => {
                self.start_turn(
                    turn.as_str(),
                    command.as_ref().map(|c| c.as_str().to_owned()),
                    now,
                    seq,
                    &mut out,
                );
                self.phase_patch(&mut patch);
            }
            KernelEvent::MessageDone { message } => self.message(message, now, seq, &mut out),
            KernelEvent::StreamDelta {
                turn,
                response,
                index,
                delta,
            } => self.stream(turn.as_str(), *response, *index, delta, now, seq, &mut out),
            KernelEvent::ToolStarted {
                tool_use,
                name,
                input,
            } => {
                let key = tool_key(tool_use.as_str());
                if !self.index.contains_key(&key) {
                    self.tool_row(tool_use.as_str(), name, input, now, seq, &mut out);
                }
                self.upsert(
                    &key,
                    |row| {
                        if row["status"] != "pendingApproval" {
                            row.insert("status".into(), json!("running"));
                        }
                        row.entry("startedAt").or_insert(json!(now));
                    },
                    &mut out,
                );
            }
            KernelEvent::ToolDone { tool_use, output } => {
                let text = output_text(output);
                let status = if self.denied.remove(tool_use.as_str()) {
                    "cancelled"
                } else if output.is_error {
                    "error"
                } else {
                    "success"
                };
                self.upsert(
                    &tool_key(tool_use.as_str()),
                    |row| {
                        row.insert("status".into(), json!(status));
                        row.insert("output".into(), json!({"text": text}));
                        row.insert("endedAt".into(), json!(now));
                        row.remove("approvalInteractionId");
                    },
                    &mut out,
                );
            }
            KernelEvent::InteractionAsked {
                interaction,
                request,
            } => {
                if let Some(tool) = self.ask(interaction.as_str(), request, now) {
                    let id = interaction.as_str().to_owned();
                    self.upsert(
                        &tool_key(&tool),
                        |row| {
                            row.insert("status".into(), json!("pendingApproval"));
                            row.insert("approvalInteractionId".into(), json!(id));
                        },
                        &mut out,
                    );
                    patch.insert("pendingInteractions".into(), json!(self.pending));
                }
            }
            KernelEvent::InteractionResolved { interaction } => {
                let before = self.pending.len();
                let tool = self
                    .pending
                    .iter()
                    .find(|pending| pending["interactionId"] == interaction.as_str())
                    .and_then(|pending| {
                        pending["payload"]["toolCallId"].as_str().map(str::to_owned)
                    });
                self.pending
                    .retain(|pending| pending["interactionId"] != interaction.as_str());
                if self.pending.len() != before {
                    patch.insert("pendingInteractions".into(), json!(self.pending));
                }
                if let Some(tool) = tool {
                    self.upsert(
                        &tool_key(&tool),
                        |row| {
                            if row["status"] == "pendingApproval" {
                                row.insert("status".into(), json!("running"));
                            }
                        },
                        &mut out,
                    );
                }
            }
            KernelEvent::TurnFinished {
                turn,
                outcome,
                usage,
            } => {
                self.finish_turn(turn.as_str(), outcome, usage.as_ref(), now, &mut out);
                self.phase_patch(&mut patch);
                patch.insert("usage".into(), state::usage(self.context, &self.total));
                if !self.pending.is_empty() {
                    self.pending.clear();
                    patch.insert("pendingInteractions".into(), json!([]));
                }
            }
            KernelEvent::ContextUsage {
                used_tokens,
                max_tokens,
            } => {
                self.context = Some((*used_tokens, *max_tokens));
                patch.insert("usage".into(), state::usage(self.context, &self.total));
            }
            KernelEvent::TitleChanged { title } => {
                self.title.clone_from(title);
                patch.insert("meta".into(), state::meta(&self.title, true));
            }
            KernelEvent::ModelChanged { selection } => {
                self.selection = selection.clone();
                patch.insert("config".into(), self.config());
            }
            KernelEvent::ModeChanged { mode } => {
                self.mode = *mode;
                patch.insert("config".into(), self.config());
            }
            KernelEvent::PlanChanged { enabled } => {
                self.plan = *enabled;
                patch.insert("config".into(), self.config());
            }
            KernelEvent::QueueChanged { items } => {
                // 立即开跑的输入（空闲时提交）在内核里也经过队列：不投成排队项；投出的值没变时不发。
                let before = self.queue_state();
                self.queue.clone_from(items);
                let after = self.queue_state();
                if after != before {
                    patch.insert("queue".into(), after);
                }
            }
            _ => {}
        }
        if event.revision != self.revision {
            self.revision = event.revision;
            patch.insert("revision".into(), json!(self.revision));
        }
        if !patch.is_empty() {
            out.push(json!({"op": "state.updated", "patch": patch}));
        }
        out
    }

    fn start_turn(
        &mut self,
        turn: &str,
        command: Option<String>,
        now: u64,
        seq: u64,
        out: &mut Vec<Value>,
    ) {
        self.executions += 1;
        self.phase = Phase::Running;
        self.error = None;
        self.active = Some(ActiveWork {
            execution: format!("runtime_command_{}", self.executions),
            command: command.clone(),
            started_at: now,
        });
        self.turn = Some(Turn {
            id: turn.to_owned(),
            command: command.clone(),
            started_at: now,
            response: None,
            keys: Vec::new(),
        });
        let mut row = self.base(turn, turn, "turnHeader", now, seq);
        row.insert("origin".into(), json!("userInput"));
        row.insert("executionKind".into(), json!("agent"));
        if let Some(command) = &command {
            row.insert("sourceCommandId".into(), json!(command));
        }
        row.insert("state".into(), json!("running"));
        row.insert("startedAt".into(), json!(now));
        self.append(turn_key(turn), row, out);
    }

    fn finish_turn(
        &mut self,
        turn: &str,
        outcome: &TurnOutcome,
        usage: Option<&Usage>,
        now: u64,
        out: &mut Vec<Value>,
    ) {
        if let Some(usage) = usage {
            state::add_usage(&mut self.total, usage);
        }
        let (header, row_state, phase) = match outcome {
            TurnOutcome::Success => ("completedSuccess", "complete", Phase::CompletedSuccess),
            TurnOutcome::Interrupted => (
                "completedInterrupted",
                "interrupted",
                Phase::CompletedInterrupted,
            ),
            TurnOutcome::Failed { message } => {
                self.error = Some(LastError {
                    message: message.clone(),
                    at: now,
                });
                ("failed", "failed", Phase::Error)
            }
        };
        self.phase = phase;
        self.active = None;
        let started = self.turn.as_ref().map_or(now, |turn| turn.started_at);
        let keys = self.turn.take().map(|turn| turn.keys).unwrap_or_default();
        for key in &keys {
            self.upsert(
                key,
                |row| match row["kind"].as_str() {
                    Some("assistantText") if row["state"] == "streaming" => {
                        row.insert("state".into(), json!(row_state));
                    }
                    Some("reasoning") if row["state"] == "streaming" => {
                        let reasoning = if row_state == "failed" {
                            "interrupted"
                        } else {
                            row_state
                        };
                        row.insert("state".into(), json!(reasoning));
                    }
                    Some("toolCall")
                        if matches!(
                            row["status"].as_str(),
                            Some("inputStreaming" | "pendingApproval" | "running")
                        ) =>
                    {
                        row.insert("status".into(), json!("cancelled"));
                        row.remove("approvalInteractionId");
                        row.insert("endedAt".into(), json!(now));
                    }
                    _ => {}
                },
                out,
            );
        }
        let output_tokens = usage.map(|usage| usage.output_tokens);
        self.upsert(
            &turn_key(turn),
            |row| {
                row.insert("state".into(), json!(header));
                row.insert("endedAt".into(), json!(now));
                row.insert("activeMs".into(), json!(now.saturating_sub(started)));
                if let Some(tokens) = output_tokens {
                    row.insert("outputTokens".into(), json!(tokens));
                }
            },
            out,
        );
    }

    /// 队列（`queueStateSchema`）：内核的排队输入加上 sendText 时记下的事实。
    fn queue_state(&self) -> Value {
        let items: Vec<Value> = self
            .queue
            .iter()
            .filter(|item| {
                self.inputs
                    .get(item.command.as_str())
                    .is_none_or(|meta| meta.admitted != "startNow")
            })
            .map(|item| {
                let command = item.command.as_str();
                let meta = self.inputs.get(command);
                let mut value = json!({
                    "sourceCommandId": command,
                    "queueItemId": format!("queue_{command}"),
                    "clientId": meta.map_or("unknown", |meta| meta.client.as_str()),
                    "kind": "sendText",
                    "text": item.text,
                    "attachments": [],
                    "delivery": {
                        "requested": meta.map_or("queue", |meta| meta.requested),
                        "admitted": meta.map_or("queue", |meta| meta.admitted),
                    },
                    "order": {"admissionSeq": 0},
                    "steer": {"state": "notRequested"},
                    "dispatch": {"state": "queued"},
                    "admittedAt": meta.map_or(self.created_at, |meta| meta.admitted_at),
                });
                if let Some(meta) = meta {
                    if let Some(origin) = &meta.origin {
                        value["inputOrigin"] = json!(origin);
                    }
                    if let Some(selection) = &meta.selection {
                        value["modelSelection"] = selection.clone();
                    }
                    if let Some(mode) = &meta.mode {
                        value["mode"] = json!(mode);
                    }
                    if let Some(plan) = meta.plan {
                        value["planEnabled"] = json!(plan);
                    }
                }
                value
            })
            .collect();
        json!({"items": items, "autoDrain": true})
    }

    /// 快照（`conversationSnapshotSchema`）：`seq` 是主题的序号，`epoch` 是主题的代际标识。
    pub fn snapshot(&self, seq: u64, epoch: &str) -> Value {
        let start = self.rows.len().saturating_sub(WINDOW_ROWS);
        json!({
            "protocolVersion": 1,
            "sessionId": self.session,
            "logEpoch": epoch,
            "seq": seq,
            "revision": self.revision,
            "control": self.control(),
            "availability": state::availability(),
            "inputRouting": state::input_routing(self.phase, self.followup),
            "meta": state::meta(&self.title, true),
            "config": self.config(),
            "modelTransition": null,
            "usage": state::usage(self.context, &self.total),
            "queue": self.queue_state(),
            "pendingInteractions": self.pending,
            "pendingCommands": [],
            "backgroundWorks": [],
            "subagents": {"revision": 0, "childSessionIds": [], "running": [], "endedTotal": 0},
            "goal": null,
            "plan": null,
            "workspaceHookAdmission": null,
            "rows": {
                "window": &self.rows[start..],
                "totalCount": self.rows.len(),
                "firstRowId": self.rows.first().map(|row| row["rowId"].clone()),
            },
        })
    }
}

#[cfg(test)]
#[path = "projection_tests.rs"]
mod tests;
