use super::{Engine, engine::Active};
use crate::contract::{Event, EventSink as Sink, ModelPort, RequestKind, RequestOrigin};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

impl Engine {
    pub(super) fn start_run(&mut self, id: &str, turn_id: String) -> Result<()> {
        self.start_run_for(id, turn_id, None)
    }
    /// `input` is the inputId acknowledged to the client when it differs from the
    /// turn's userInput row (inputs promoted from the queue use their queue item id).
    pub(super) fn start_run_for(
        &mut self,
        id: &str,
        turn_id: String,
        input: Option<String>,
    ) -> Result<()> {
        let origin = self.run_origin(id, &turn_id, input)?;
        let identity = self.session_selection(id)?;
        let (selection, updates) = tokio::sync::watch::channel(identity.clone());
        let model: Arc<dyn ModelPort> = if let Some(registry) = &self.registry {
            registry.resolve(&identity)?;
            Arc::new(super::model_config::LiveModel {
                registry: registry.clone(),
                selection: updates,
            })
        } else {
            self.model.clone().context("Model configuration required")?
        };
        let session = self.sessions.get_mut(id).context("Session unavailable")?;
        tracing::info!(
            target: "zcode::runtime",
            event = "run.started",
            session_id = id,
            provider = identity.provider_id.as_str(),
            model = identity.model_id.as_str(),
            "Run started"
        );
        let estimated = session.active_context_tokens();
        let run_id = session.run_id.clone().context("Run reservation required")?;
        let cancel = CancellationToken::new();
        self.active.insert(
            id.into(),
            Active {
                selection,
                cancel: cancel.clone(),
                run_id: run_id.clone(),
                turn_id,
                origin: origin.clone(),
            },
        );
        let manual = session
            .rows
            .last()
            .filter(|r| r["kind"] == "turnHeader" && r["executionKind"] == "controlOnly")
            .map(|_| session.compact_instructions.clone().unwrap_or_default());
        let mut history = super::context::RunContext::new(
            session.context.clone(),
            session.messages[session.context.offset..].to_vec(),
            manual,
            estimated,
        );
        history.prompt_snapshot = session.prompt_snapshot.clone();
        history.skills = session.skills.clone();
        history.goal = session.goal.clone();
        history.agent_profile = session.agent_profile.clone();
        let context = self.context.clone();
        let tools = self.tools.clone();
        let sink = Sink {
            session_id: id.into(),
            run_id,
            tx: self.events.clone(),
            origin,
        };
        tokio::spawn(async move {
            let result = super::agent_loop::run(
                model.as_ref(),
                tools.as_ref(),
                context.as_ref(),
                &mut history,
                &sink,
                &cancel,
            )
            .await;
            let model_failure = result
                .as_ref()
                .err()
                .and_then(|e| e.downcast_ref::<crate::contract::ModelFailure>())
                .cloned();
            let error = result.err().map(|e| e.to_string());
            let _ = sink
                .send(Event::Finished {
                    error,
                    model_failure,
                    cancelled: cancel.is_cancelled(),
                })
                .await;
        });
        Ok(())
    }

    /// Node model attribution: the session runtime's root trace (created once per
    /// process, never persisted) and the query of the input that started the turn.
    /// Subagents inherit the parent run's trace and query.
    fn run_origin(
        &mut self,
        id: &str,
        turn: &str,
        input: Option<String>,
    ) -> Result<Arc<RequestOrigin>> {
        let session = self.sessions.get_mut(id).context("Session unavailable")?;
        let parent = session
            .parent_id
            .as_deref()
            .and_then(|parent| self.active.get(parent))
            .map(|active| active.origin.clone());
        let origin = match parent {
            Some(parent) => RequestOrigin {
                kind: RequestKind::Subagent,
                session_id: Some(id.into()),
                trace_id: parent.trace_id.clone(),
                query_id: parent.query_id.clone(),
            },
            None => {
                let query = input.or_else(|| {
                    session
                        .rows
                        .iter()
                        .rev()
                        .find(|r| r["kind"] == "userInput" && r["turnId"] == turn)
                        .and_then(|r| r["entityId"].as_str())
                        .map(str::to_owned)
                });
                RequestOrigin {
                    kind: if session.parent_id.is_some() {
                        RequestKind::Subagent
                    } else {
                        RequestKind::Main
                    },
                    session_id: Some(id.into()),
                    trace_id: session
                        .runtime_trace
                        .get_or_insert_with(|| self.clock.id())
                        .clone(),
                    query_id: Some(query.unwrap_or_else(|| turn.into())),
                }
            }
        };
        Ok(Arc::new(origin))
    }
}
