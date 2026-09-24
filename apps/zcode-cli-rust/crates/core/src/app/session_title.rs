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

/// Node `turnNumber === 0`: no real prompt before this one.
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
        .count();
    prompts <= 1 && s.context.summary.is_none()
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
        if !self.start_title(id, objective, message, Some(target.clone()), false) {
            self.goal_summary_title(id, objective, &target);
        }
    }

    /// The first prompt's deferred title, once its turn completed.
    pub(super) fn title_turn_end(&mut self, id: &str, event: &Event) {
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
        let Some(s) = self.sessions.get_mut(id).filter(|_| completed) else {
            return;
        };
        if let Some((input, message)) = s.runtime.title_deferred.take() {
            self.start_title(id, &input, message, None, false);
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
        if defer && model.as_ref().is_none_or(|(_, auth)| *auth) {
            // Node：需要先刷新运行时请求头的 provider 让本轮先发出，结束后再补标题。
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
        let logical = format!("{source}_{}", self.clock.id());
        let trace = s.runtime_trace.clone().unwrap_or_else(|| self.clock.id());
        let fact = ModelFact {
            id: format!("usage_model_{source}_{logical}_0"),
            logical_request_id: logical,
            session_id: id.into(),
            turn_id: s.node.turn.as_ref().map(|t| t.runtime.clone()),
            trace_id: Some(trace.clone()),
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
        let origin = RequestOrigin {
            kind: RequestKind::Other,
            session_id: Some(id.into()),
            trace_id: trace,
            query_id: None,
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
                        "usage": out.usage, "limit": out.output_limit})
                }),
                Err(_) => Err(ModelFailure::new("timeout", true)),
            };
            let _ = sink.send(Event::AuxiliaryDone { result }).await;
        });
    }
}
