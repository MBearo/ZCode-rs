use super::{Engine, engine::Active};
use crate::contract::{Event, EventSink as Sink, ModelPort, RequestKind, RequestOrigin};
use anyhow::{Context, Result};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

impl Engine {
    pub(super) fn start_run(&mut self, id: &str, turn_id: String) -> Result<()> {
        let origin = self.run_origin(id, &turn_id)?;
        // 未经 admit_input 的续跑（子代理完成、目标续跑）没有本轮选项，按默认处理。
        let submission = self
            .submissions
            .remove(&(id.to_owned(), turn_id.clone()))
            .unwrap_or_default();
        let identity = match &submission.execution {
            Some(execution) => execution.selection.clone(),
            None => self.session_selection(id)?,
        };
        let (selection, updates) = tokio::sync::watch::channel(identity.clone());
        let model: Arc<dyn ModelPort> = if let Some(registry) = &self.registry {
            // 缺少推理档位时本轮照常开始，由模型构建失败收口（与 Node 一致）。
            if !identity.reasoning_level.is_empty() {
                registry.resolve(&identity)?;
            }
            Arc::new(super::model_config::LiveModel {
                registry: registry.clone(),
                selection: updates,
            })
        } else {
            self.model.clone().context("Model configuration required")?
        };
        let (permissions, permission_updates) =
            tokio::sync::watch::channel(self.permission_snapshot(id));
        let session = self.sessions.get_mut(id).context("Session unavailable")?;
        tracing::info!(
            target: "zcode::runtime",
            event = "run.started",
            session_id = id,
            provider = identity.provider_id.as_str(),
            model = identity.model_id.as_str(),
            execution_scoped = submission.execution.is_some(),
            memory_extraction_skipped = submission.execution.as_ref().is_some_and(|e| e.memory_skip),
            automation_id = submission.automation_id.as_deref(),
            off_peak_task_id = submission.off_peak_task_id.as_deref(),
            off_peak_run_type = submission.off_peak_run_type.as_deref(),
            "Run started"
        );
        let kind = super::legacy_input::run_kind(session, &turn_id);
        let estimated = session.active_context_tokens();
        let run_id = session.run_id.clone().context("Run reservation required")?;
        let who = crate::domain::usage::Attribution {
            session_id: id.into(),
            run_id: run_id.clone(),
            turn_id: turn_id.clone(),
            trace_id: origin.trace_id.clone(),
            variant: Some(identity.reasoning_level.clone()).filter(|level| !level.is_empty()),
            mode: session.mode.as_str().into(),
            subagent: session.parent_id.is_some(),
            compact: kind == crate::domain::legacy_stream::RunKind::Compact,
        };
        let cancel = CancellationToken::new();
        self.active.insert(
            id.into(),
            Active {
                selection,
                cancel: cancel.clone(),
                run_id: run_id.clone(),
                turn_id: turn_id.clone(),
                origin: origin.clone(),
                execution: submission.execution.clone(),
                permissions,
                kind,
                legacy_lock: kind != crate::domain::legacy_stream::RunKind::Prompt,
                step: Default::default(),
                usage: crate::domain::usage::RunUsage::new(who, self.clock.now()),
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
        history.tool_disallowlist = submission.tool_disallowlist;
        history.tool_filter = session.runtime.tools.clone();
        history.anomaly_guard = self.anomaly_guard;
        history.compact_failures = session.runtime.compact_failures;
        // Node turnNumber：本轮输入之前已有用户消息（或已压缩）即非首轮。
        let users = session
            .messages
            .iter()
            .filter(|m| {
                m["role"] == "user"
                    && !m["content"]
                        .as_str()
                        .is_some_and(|c| c.starts_with("<system-reminder>"))
            })
            .count();
        history.returning = users > 1 || session.context.summary.is_some();
        // Node 的提醒（plan、hook 上下文）留在进程内历史中；新一轮按原位置继续发送（重启后清空）。
        let offset = session.context.offset;
        history.restore_transient(session.runtime.reminders.iter().filter_map(
            |(anchor, kind, message)| {
                Some((
                    anchor.checked_sub(offset)?,
                    super::context::TransientKind::Reminder(*kind),
                    message.clone(),
                ))
            },
        ));
        history.permissions = Some(permission_updates);
        self.legacy_turn_started(id, &turn_id, kind);
        // Node 只解析用户可见输入的原文；子代理与模型专用续跑不产生引用提醒。
        if self.sessions[id].parent_id.is_none()
            && let Some((text, _)) = &submission.prompt
        {
            history.plugin_references = crate::domain::plugin_reference::extract(text);
        }
        let turn_hooks = self.turn_hooks(id, &turn_id, submission.prompt, &identity);
        let context = self.context.clone();
        let tools = self.tools.clone();
        let sink = Sink {
            session_id: id.into(),
            run_id,
            tx: self.events.clone(),
            origin,
            request_auth: submission
                .execution
                .as_ref()
                .and_then(|e| e.request_auth.clone()),
        };
        tokio::spawn(async move {
            let result = super::agent_loop::run(
                model.as_ref(),
                tools.as_ref(),
                context.as_ref(),
                &mut history,
                turn_hooks,
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

    /// The run's hooks (none in subagents, Node builds subagent runtimes
    /// without hooks). SessionStart runs once per session and process.
    fn turn_hooks(
        &mut self,
        id: &str,
        turn: &str,
        prompt: Option<(String, Option<String>)>,
        identity: &crate::contract::ModelIdentity,
    ) -> Option<super::turn_hooks::TurnHooks> {
        let session = self.sessions.get_mut(id)?;
        let first = !std::mem::replace(&mut session.runtime.session_start_ran, true);
        // 会话首轮尚未解析插件 hooks 时也要进入 TurnHooks，由运行向 engine 取回。
        let without_plugins = self.hooks.plugins.get(id).is_some_and(Option::is_none);
        if session.parent_id.is_some()
            || self.hooks.user.is_empty() && self.hooks.ports.is_none() && without_plugins
        {
            return None;
        }
        let at = session
            .history
            .inputs
            .iter()
            .rev()
            .find(|input| input.turn == turn)
            .map(|input| input.message);
        // 本进程首次开轮：会话此前已有输入即视为恢复（Node resume 的 SessionStart）。
        let source = if session.history.inputs.len() > 1 {
            "resume"
        } else {
            "startup"
        };
        let hooks = super::hook_runner::Hooks {
            registrations: self.hooks.user.clone(),
            admission: None,
            tools: self.tools.clone(),
            clock: self.clock.clone(),
            cwd: self.workspace_path.clone(),
            turn_id: turn.into(),
        };
        Some(super::turn_hooks::TurnHooks {
            hooks: Arc::new(hooks),
            session_start: first.then_some(source),
            prompt: prompt
                .zip(at)
                .map(|((text, attachments), at)| super::turn_hooks::Prompt {
                    text,
                    attachments,
                    at,
                }),
            model: format!("{}/{}", identity.provider_id, identity.model_id),
            stop_continuations: 0,
            tool_calls: 0,
        })
    }

    /// Node model attribution: the session runtime's root trace (created once per
    /// process, never persisted) and the query of the input that started the turn,
    /// which is the command id that submitted it. Subagents inherit the parent run.
    fn run_origin(&mut self, id: &str, turn: &str) -> Result<Arc<RequestOrigin>> {
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
                query_source: "subagent",
                stream_recovery: None,
            },
            None => {
                let query = session
                    .rows
                    .iter()
                    .rev()
                    .find(|r| r["kind"] == "userInput" && r["turnId"] == turn)
                    .and_then(|r| r["sourceCommandId"].as_str())
                    .map(str::to_owned);
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
                    query_source: if session.parent_id.is_some() {
                        "subagent"
                    } else {
                        "main_turn"
                    },
                    stream_recovery: None,
                }
            }
        };
        Ok(Arc::new(origin))
    }
}
