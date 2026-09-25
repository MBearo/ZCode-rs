//! Generated session and goal summary titles (Node `session-title.ts`,
//! `title-generation-sidecar.ts`, `goal-summary-title.ts`; spec
//! rust-m11-node-storage §5.2): a detached auxiliary request once the first
//! prompt is stored; its answer updates the session row (`titleSource =
//! generated`) and the goal's summary title.
use super::{Engine, auxiliary::Auxiliary};
use crate::contract::{Event, EventSink, ModelFailure, ModelPort, RequestKind, RequestOrigin};
use crate::domain::session_title::{
    self as title, GOAL_SUMMARY_TITLE_SOURCE, SESSION_TITLE_SOURCE,
};
use crate::domain::usage::ModelFact;
use serde_json::json;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub(super) struct TitleJob {
    pub session: String,
    /// The first prompt's stored message (Node `titleMessageID`).
    pub message: Option<String>,
    pub input: String,
    /// Writes the session title (else only the goal summary title).
    pub session_title: bool,
    /// The goal whose summary title the answer also sets.
    pub goal: Option<String>,
    pub started_at: u64,
    pub fact: ModelFact,
}

/// Node `turnNumber === 0`: no prompt stored before this activation and no
/// turn completed in it (a failed or cancelled first turn does not count).
fn first_turn(s: &crate::domain::session::Session) -> bool {
    let prompts = s
        .messages
        .iter()
        .filter(|m| {
            m["role"] == "user"
                && !m["content"]
                    .as_str()
                    .is_some_and(|c| c.starts_with("<system-reminder>"))
        })
        .count() as u64;
    let before = prompts.saturating_sub(s.runtime.prompts_started);
    before + s.runtime.turns_completed == 0 && s.context.summary.is_none()
}

impl Engine {
    /// The auxiliary model of the session's selection (Node `createRuntimeModel`
    /// bound with `auxiliaryModelOptions`); `None` without a selection.
    fn title_model(&self, id: &str) -> Option<(Arc<dyn ModelPort>, bool)> {
        let selection = self.session_selection(id).ok()?;
        if selection.provider_id.is_empty() || selection.model_id.is_empty() {
            return None;
        }
        let base: Arc<dyn ModelPort> = match &self.registry {
            Some(registry) => Arc::new(super::model_config::LiveModel {
                registry: registry.clone(),
                selection: tokio::sync::watch::channel(selection).1,
            }),
            None => self.model.clone()?,
        };
        let auth = base.account_auth();
        Some((base.auxiliary().unwrap_or(base), auth))
    }

    /// Node `maybeStartSessionTitleGeneration` for the prompt of `turn` (a
    /// real input starting a run); `defer` postpones account providers until
    /// the turn ends.
    pub(super) fn prompt_title(&mut self, id: &str, turn: &str) {
        let Some(s) = self.sessions.get(id) else {
            return;
        };
        let Some(input) = s
            .rows
            .iter()
            .rev()
            .find(|r| r["kind"] == "userInput" && r["turnId"] == turn && r["origin"] == "realUser")
            .and_then(|r| r["text"].as_str())
            .map(str::to_owned)
        else {
            return;
        };
        let message = s.history.inputs.last().and_then(|i| i.node_message.clone());
        self.sessions.get_mut(id).unwrap().runtime.prompts_started += 1;
        self.start_title(id, &input, message, None, true);
    }

    /// Node `recordExternalUserPrompt`'s title of a `/goal`: the session title
    /// with the goal summary title, else the goal summary title alone.
    pub(super) fn goal_title(&mut self, id: &str, objective: &str) {
        let Some(s) = self.sessions.get(id) else {
            return;
        };
        let message = s.history.inputs.last().and_then(|i| i.node_message.clone());
        let Some(target) = s.goal.as_ref().map(|g| g.target_id.clone()) else {
            return;
        };
        // 目标的 objective 是本次激活的输入，不算之前的轮次（Node turnNumber 仍为 0）。
        self.sessions.get_mut(id).unwrap().runtime.prompts_started += 1;
        if !self.start_title(id, objective, message, Some(target.clone()), false) {
            self.goal_summary_title(id, objective, &target);
        }
    }

    /// The first prompt's deferred title, once its turn completed; the run
    /// counts toward Node's `turnNumber` after it (a compaction does not).
    pub(super) fn title_turn_end(&mut self, id: &str, event: &Event) {
        if !matches!(event, Event::Finished { .. }) {
            return;
        }
        let completed = matches!(
            event,
            Event::Finished {
                error: None,
                cancelled: false,
                ..
            }
        ) && self
            .active
            .get(id)
            .is_some_and(|a| !a.cancel.is_cancelled());
        let compaction = self
            .active
            .get(id)
            .is_some_and(|a| a.kind == crate::domain::legacy_stream::RunKind::Compact);
        let Some(s) = self.sessions.get_mut(id) else {
            return;
        };
        // Node 只在本轮成功后补发推迟的标题；失败或取消的轮次不留到之后的其他 run。
        let deferred = s.runtime.title_deferred.take();
        if !completed {
            return;
        }
        if let Some((input, message)) = deferred {
            self.start_title(id, &input, message, None, false);
        }
        if !compaction && let Some(s) = self.sessions.get_mut(id) {
            s.runtime.turns_completed += 1;
        }
    }

    /// Node `shouldAttemptSessionTitleGeneration` and the sidecar start.
    fn start_title(
        &mut self,
        id: &str,
        input: &str,
        message: Option<String>,
        goal: Option<String>,
        defer: bool,
    ) -> bool {
        let Some(s) = self.sessions.get(id) else {
            return false;
        };
        if !self.titles
            || s.runtime.title_attempted
            || s.runtime.title_generation_disabled
            || s.parent_id.is_some()
            || s.task_type != "interactive"
            || !first_turn(s)
            || !title::input_eligible(input, false)
        {
            return false;
        }
        let model = self.title_model(id);
        if defer {
            // Node 协议模式的 providerRuntimeHeadersPort.shouldRefreshBeforeModelRequest 恒为
            // true：首条输入的标题总是等本轮成功结束后再生成（原先只对账号类供应商推迟）。
            let s = self.sessions.get_mut(id).unwrap();
            s.runtime.title_deferred = Some((input.into(), message));
            return false;
        }
        let s = self.sessions.get_mut(id).unwrap();
        s.runtime.title_attempted = true;
        // Node：自定义标题且没有目标摘要时不发请求。
        let session_title = s.title_source != "custom";
        let Some((model, _)) = model.filter(|_| session_title || goal.is_some()) else {
            return true;
        };
        self.spawn_title(id, model, (input, message), (session_title, goal));
        true
    }

    /// Node `maybeStartGoalSummaryTitleGeneration`: the objective itself when
    /// generation is not possible.
    fn goal_summary_title(&mut self, id: &str, objective: &str, target: &str) {
        let Some(s) = self.sessions.get(id) else {
            return;
        };
        let eligible = self.titles
            && !s.runtime.title_generation_disabled
            && s.parent_id.is_none()
            && s.task_type == "interactive"
            && !target.trim().is_empty()
            && !title::normalize(objective).is_empty();
        match self.title_model(id).filter(|_| eligible) {
            Some((model, _)) => {
                let job = (objective, None);
                self.spawn_title(id, model, job, (false, Some(target.into())));
            }
            None => self.set_goal_summary(id, target, title::fallback_goal_title(objective), true),
        }
    }

    fn spawn_title(
        &mut self,
        id: &str,
        model: Arc<dyn ModelPort>,
        (input, message): (&str, Option<String>),
        (session_title, goal): (bool, Option<String>),
    ) {
        let s = &self.sessions[id];
        let source = if session_title {
            SESSION_TITLE_SOURCE
        } else {
            GOAL_SUMMARY_TITLE_SOURCE
        };
        let now = self.clock.now();
        let identity = model
            .identity()
            .unwrap_or_else(|| self.session_selection(id).unwrap());
        // Node：标题请求以 span id 为逻辑请求 id，归属触发它的轮次与 user 消息。
        let span: String = self.clock.id().chars().take(16).collect();
        let active = self.active.get(id);
        let trace = active
            .map(|a| a.origin.trace_id.clone())
            .or_else(|| s.runtime_trace.clone())
            .unwrap_or_else(|| self.clock.id());
        let fact = ModelFact {
            id: format!("usage_model_{source}_{span}_0"),
            logical_request_id: span.clone(),
            session_id: id.into(),
            turn_id: active.map(|a| a.turn_id.clone()),
            trace_id: Some(trace.clone()),
            span_id: Some(span),
            parent_user_message_id: message.clone(),
            query_source: source.into(),
            provider_id: identity.provider_id.clone(),
            model_id: identity.model_id.clone(),
            variant: Some(identity.reasoning_level.clone()).filter(|l| !l.is_empty()),
            agent: "zcode-agent".into(),
            mode: s.mode.as_str().into(),
            task_type: "interactive".into(),
            started_at: now,
            ..ModelFact::default()
        };
        let job = TitleJob {
            session: id.into(),
            message,
            input: input.into(),
            session_title,
            goal,
            started_at: now,
            fact,
        };
        let key = format!("session-title:{}", self.clock.id());
        let cancel = CancellationToken::new();
        self.auxiliary.insert(
            key.clone(),
            Auxiliary {
                token: 0,
                cancel: cancel.clone(),
                operation: None,
                plugin_operation: None,
                title: Some(Box::new(job)),
            },
        );
        // Node 的标题请求沿用触发轮次的 queryId（网络状态与遥测据此归属）。
        let query = self.active.get(id).and_then(|a| a.origin.query_id.clone());
        let origin = RequestOrigin {
            kind: RequestKind::Other,
            session_id: Some(id.into()),
            trace_id: trace,
            query_id: query,
            query_source: source,
            stream_recovery: None,
        };
        let sink = EventSink {
            session_id: key.clone(),
            run_id: key,
            tx: self.events.clone(),
            origin: Arc::new(origin),
            request_auth: None,
        };
        let messages = title::messages(input);
        tokio::spawn(async move {
            let timeout = std::time::Duration::from_millis(title::TIMEOUT_MS);
            let request = model.complete(messages, &[], &sink, &cancel);
            let result = match tokio::time::timeout(timeout, request).await {
                Ok(result) => result.map(|out| {
                    json!({"text": out.message["content"], "calls": out.calls.len(),
                        "usage": out.usage, "limit": out.output_limit,
                        "rawFinish": out.raw_finish_reason})
                }),
                Err(_) => Err(ModelFailure::new("timeout", true)),
            };
            let _ = sink.send(Event::AuxiliaryDone { result }).await;
        });
    }
}
