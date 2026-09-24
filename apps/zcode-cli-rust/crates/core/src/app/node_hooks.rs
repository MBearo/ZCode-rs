//! Node session records of the engine's facts (spec rust-m11-node-storage
//! §5.1–5.2): the journal hooks run at the moments Node persists, only when
//! the store keeps Node records. The next `persist` commits what they queue.
use super::{Engine, Event};
use crate::domain::node_journal::{self as nj, Admission, Outcome, Prompt, intent};
use crate::domain::{protocol::Command, session::Session};
use anyhow::Result;
use serde_json::{Value, json};

/// The CLI version Node stores on the session row.
const VERSION: &str = env!("CARGO_PKG_VERSION");

impl Engine {
    pub(super) fn journaling(&self) -> bool {
        self.store.node_journal()
    }

    /// The session's facts are written as Node records: a root session, or a
    /// child already stored (a fork). 子代理子会话尚未按 Node 规则落库，过渡期不写，
    /// 避免产生缺少会话行的消息。
    pub(super) fn journaled(&self, id: &str) -> bool {
        self.journaling()
            && self
                .sessions
                .get(id)
                .is_some_and(|s| s.parent_id.is_none() || s.node.created)
    }

    /// Runs `f` on the session's journal when the store keeps Node records.
    pub(super) fn node(&mut self, id: &str, f: impl FnOnce(&mut Session, u64)) {
        if !self.journaled(id) {
            return;
        }
        let now = self.clock.now();
        if let Some(s) = self.sessions.get_mut(id) {
            f(s, now);
        }
    }

    /// A new session id (Node `createSessionId` when Node records are kept).
    pub(super) fn new_session_id(&self) -> String {
        let id = self.clock.id();
        if self.journaling() {
            crate::domain::node_ids::session_id(&id)
        } else {
            id
        }
    }

    /// Node V4 `admitInputCommand` for an input that starts now (the session
    /// row first); returns the input's intent for its user message.
    pub(super) fn node_admit_now(&mut self, id: &str, c: &Command, source: &str) -> Option<Value> {
        if !self.journaled(id) {
            return None;
        }
        let now = self.clock.now();
        let s = self.sessions.get_mut(id)?;
        let mut item = super::commands::queue_item(c, s, now);
        let requested = c.payload["requestedDelivery"]
            .as_str()
            .unwrap_or("startNow");
        item["delivery"] = json!({"requested": requested, "admitted": "startNow"});
        if c.kind == "createSession" {
            item["kind"] = "sendText".into();
        }
        if c.payload.get("attachments").is_none() {
            item.as_object_mut()?.remove("attachments");
        }
        // 编辑/重试重跑：Node inputIntentMetadataFromCanonical 保留原输入来源。
        if let Some(provenance) = c.payload.get("_provenance") {
            item["provenance"] = provenance.clone();
        }
        s.node_ensure_created(now, c.payload["text"].as_str().unwrap_or(""), VERSION);
        let queue_id = item["queueItemId"].as_str()?.to_owned();
        s.node_admit_input(
            now,
            Admission {
                queue_id: &queue_id,
                kind: item["kind"].as_str()?,
                payload: intent::admission_payload(&item, source),
                delivery: "startNow",
            },
        );
        Some(intent::turn_intent(&item, false))
    }

    /// Node V4 admission of a busy input: the ledger row of the queued (or
    /// reserved start-now) item.
    pub(super) fn node_queue(&mut self, id: &str, item: &Value) {
        self.node(id, |s, now| {
            s.node_ensure_created(now, item["text"].as_str().unwrap_or(""), VERSION);
            let delivery = item["delivery"]["admitted"].as_str().unwrap_or("queue");
            let payload = if delivery == "startNow" {
                intent::admission_payload(item, "sendText")
            } else {
                intent::queued_payload(item)
            };
            s.node_admit_input(
                now,
                Admission {
                    queue_id: item["queueItemId"].as_str().unwrap_or(""),
                    kind: item["kind"].as_str().unwrap_or("sendText"),
                    payload,
                    delivery,
                },
            );
        });
    }

    /// Node `persistUserPrompt` of a started input, promoted from its ledger row.
    pub(super) fn node_prompt(
        &mut self,
        id: &str,
        turn: &str,
        c: &Command,
        (intent, presentation): (Option<Value>, Option<&str>),
    ) {
        let Some(intent) = intent.filter(|_| self.journaled(id)) else {
            return;
        };
        let now = self.clock.now();
        let (message, part) = (self.clock.id(), self.clock.id());
        let refs = c.payload["attachments"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let file_ids: Vec<String> = refs.iter().map(|_| self.clock.id()).collect();
        let tools = self.tool_names();
        if !self.sessions.contains_key(id) {
            return;
        }
        let text = c.payload["text"].as_str().unwrap_or("");
        let metadata = intent::prompt_metadata(text, &intent, (None, presentation));
        if c.kind == "sendGoalCommand" {
            let ids = (
                message,
                part,
                self.clock.id(),
                self.clock.id(),
                self.clock.id(),
            );
            self.node_goal_prompt(id, turn, c, metadata, ids, &tools);
            return;
        }
        let s = self.sessions.get_mut(id).unwrap();
        let message = nj::message_id(now, &message);
        // Node persistUserPrompt：每个附件一个 file part，跟在 text part 之后。
        let files = refs
            .iter()
            .zip(&file_ids)
            .filter_map(|(item, part)| {
                let file = s.attachments.get(item["ref"].as_str()?)?.node.as_ref()?;
                Some(file.record(&nj::part_id(now, part), &s.id, &message))
            })
            .collect();
        if let Some(boundary) = s.history.inputs.last_mut() {
            boundary.node_message = Some(message.clone());
        }
        s.node_user_prompt(
            now,
            Prompt {
                message,
                part: nj::part_id(now, &part),
                turn,
                text,
                command: Some(&c.command_id),
                queue_id: intent["queueItemId"].as_str(),
                metadata: Some(metadata),
                tools: &tools,
                files,
            },
        );
    }

    /// Node subagent child start (`ensureSessionPersistedForExternalActivity`
    /// with the task prompt, then the child's model as a pending model change).
    pub(super) fn node_child_created(&mut self, child: &str, prompt: &str) {
        if !self.journaling() {
            return;
        }
        let now = self.clock.now();
        let request = nj::part_id(now, &self.clock.id());
        let Some(s) = self.sessions.get_mut(child) else {
            return;
        };
        s.node_ensure_created(now, prompt, VERSION);
        let to = s.node_selection().unwrap_or_default();
        s.node_record_model_change(request, None, to);
    }

    /// The child's task prompt (Node `executeTurn` with `coordinator_input`).
    pub(super) fn node_child_prompt(&mut self, child: &str, turn: &str, prompt: &str) {
        if !self.journaled(child) {
            return;
        }
        let now = self.clock.now();
        let (message, part) = (self.clock.id(), self.clock.id());
        let tools = self.tool_names();
        let s = self.sessions.get_mut(child).unwrap();
        s.node_user_prompt(
            now,
            Prompt {
                message: nj::message_id(now, &message),
                part: nj::part_id(now, &part),
                turn,
                text: prompt,
                command: None,
                queue_id: None,
                metadata: Some(json!({"inputPresentation": "coordinator_input"})),
                tools: &tools,
                files: vec![],
            },
        );
    }

    /// Node `persistUserPrompt`'s `tools`: the tools offered to the model.
    pub(super) fn tool_names(&self) -> Vec<String> {
        self.tools
            .definitions()
            .iter()
            .filter_map(|d| d["function"]["name"].as_str().or(d["name"].as_str()))
            .map(str::to_owned)
            .collect()
    }

    /// Node guide drain: the guided queue item's user message joins the
    /// running turn (`turnSteerDelivery = guide`), promoted from its ledger row.
    pub(super) fn node_guide(&mut self, id: &str, item: &Value) {
        if !self.journaled(id) {
            return;
        }
        let now = self.clock.now();
        let (message, part) = (self.clock.id(), self.clock.id());
        let tools = self.tool_names();
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        let intent = intent::turn_intent(item, true);
        let text = item["text"].as_str().unwrap_or("");
        // Rust 运行时以 user_steer 呈现引导输入（busy_input::drain_guide）；落库同样标注，
        // 冷读取才能还原同一条模型消息，Node 读取时也按插话呈现。
        let attachments = item["attachments"]
            .as_array()
            .is_some_and(|a| !a.is_empty());
        let presentation = (!attachments).then_some("user_steer");
        let metadata = intent::prompt_metadata(text, &intent, (Some("guide"), presentation));
        let message = nj::message_id(now, &message);
        if let Some(boundary) = s.history.inputs.last_mut() {
            boundary.node_message = Some(message.clone());
        }
        s.node_guided_prompt(
            now,
            Prompt {
                message,
                part: nj::part_id(now, &part),
                turn: "",
                text,
                command: item["sourceCommandId"].as_str(),
                queue_id: item["queueItemId"].as_str(),
                metadata: Some(metadata),
                tools: &tools,
                files: vec![],
            },
        );
    }

    /// Usage, Node records and the step probe of one run event. A new model
    /// step's assistant is stored before its request streams (Node
    /// `runModelBackedTurnStep`).
    pub(super) async fn observe_event(&mut self, id: &str, event: &Event) -> Result<()> {
        self.observe_usage(id, event).await;
        self.goal_turn_end(id, event);
        let media = self.node_tool_media(id, event).await?;
        let flush = self.node_event(id, event, media.as_ref());
        self.observe_step(id, event)?;
        if flush {
            self.persist(id, None).await?;
        }
        Ok(())
    }

    fn node_event(
        &mut self,
        id: &str,
        event: &Event,
        media: Option<&nj::tool_media::ToolMedia>,
    ) -> bool {
        if !self.journaled(id) {
            return false;
        }
        let now = self.clock.now();
        let clock = self.clock.clone();
        let mut ids = move || clock.id();
        let (Some(active), Some(s)) = (self.active.get(id), self.sessions.get_mut(id)) else {
            return false;
        };
        let cancelled = active.cancel.is_cancelled();
        match event {
            Event::ModelStatus(status)
                if matches!(
                    status["querySource"].as_str(),
                    Some("main_turn" | "subagent")
                ) =>
            {
                if status["type"] == "model_request_started" && !s.node_step_open() {
                    let step = (nj::message_id(now, &ids()), nj::part_id(now, &ids()));
                    let text = |key: &str| status[key].as_str().unwrap_or("").to_owned();
                    s.node_step_started(now, step, &text("providerId"), &text("modelId"));
                    return s.node_step_open();
                }
                s.node_model_status(status);
            }
            Event::StreamRecovery { retry, .. } => s.node_stream_discarded(now, *retry),
            Event::ModelDone { message, .. } if !cancelled => {
                s.node_model_done(now, message.as_ref(), &mut ids);
            }
            Event::ToolStart { call } if !cancelled => {
                s.node_tool_started(now, call["id"].as_str().unwrap_or(""));
            }
            Event::ToolDone {
                id: call,
                result,
                model_content,
                display,
                failed,
                ..
            } if !cancelled => {
                // Node 的 output 是模型内容的文本形态（媒体为 [Attached …] 占位）。
                let content = match model_content {
                    Some(content) => nj::tool_media::output_text(content),
                    None => result.clone(),
                };
                let result = (&content.into(), display.as_ref(), *failed);
                s.node_tool_done(now, call, result, media, &mut ids);
            }
            Event::Finished {
                error,
                model_failure,
                cancelled: stopped,
            } => {
                if *stopped || cancelled || error.is_some() {
                    let results = s.unfinished_tool_results();
                    s.node_close_tools(now, &results);
                    // Node finishCompactTimelineFailure：运行结束时未完成的压缩收口。
                    let status = if *stopped || cancelled {
                        "interrupted"
                    } else {
                        "failed"
                    };
                    s.node_compact_ended(now, status, None);
                }
                let (text, reasoning) = super::stream_recovery::streamed_output(s, &active.step);
                let outcome = if *stopped || cancelled {
                    Outcome::Cancelled {
                        text: &text,
                        reasoning: &reasoning,
                    }
                } else if let Some(error) = error {
                    let (name, data) = failure_record(s, error, model_failure.as_ref(), now);
                    Outcome::Failed { name, data }
                } else {
                    Outcome::Success
                };
                s.node_finished(now, outcome, &mut ids);
            }
            _ => {}
        }
        false
    }
}

/// Node's persisted turn error (`turn-model-step.ts`): the model adapter's
/// error with its code and attribution, else a plain runtime error.
fn failure_record(
    s: &Session,
    error: &str,
    failure: Option<&crate::contract::ModelFailure>,
    now: u64,
) -> (&'static str, Value) {
    match failure {
        Some(failure) => {
            let attribution =
                failure.last_error((&s.provider, &s.model), now)["attribution"].clone();
            let data = json!({"message": failure.message, "code": failure.code, "attribution": attribution});
            ("AiSdkModelAdapterError", data)
        }
        None => ("Error", json!({"message": error})),
    }
}
