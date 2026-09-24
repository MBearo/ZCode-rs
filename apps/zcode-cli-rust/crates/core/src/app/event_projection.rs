use super::{Engine, Event, RunEvent};
use anyhow::{Result, bail};
use serde_json::json;
impl Engine {
    pub(super) async fn apply_event(&mut self, event: RunEvent) -> Result<()> {
        if self.auxiliary.contains_key(&event.session_id) {
            return self.auxiliary_event(event);
        }
        if let Event::ToolCleanupFailed(message) = event.event {
            return self.cleanup_failed(&event.session_id, &event.run_id, message);
        }
        let Some(event) = self.hook_side(event).await? else {
            return Ok(());
        };
        if let Event::Background { task, committed } = event.event {
            return self
                .background_event(&event.session_id, &event.run_id, task, committed)
                .await;
        }
        let id = event.session_id;
        let Some(active) = self.active.get(&id) else {
            return Ok(());
        };
        if active.run_id != event.run_id {
            return Ok(());
        }
        self.observe_usage(&id, &event.event).await;
        self.observe_retry(&id, &event.event)?;
        let active = &self.active[&id];
        // 取消后的 failed(cancelled) 状态也要记入 session/debug（Node 同样发出）。
        if let Event::ModelStatus(status) = event.event {
            let turn = active.turn_id.clone();
            self.model_status(&id, &turn, status);
            return Ok(());
        }
        if let Event::StreamRecovery { retry, reply } = event.event {
            let turn = active.turn_id.clone();
            return self.stream_recovery(&id, &turn, retry, reply).await;
        }
        let cancelled = active.cancel.is_cancelled();
        if cancelled && !matches!(event.event, Event::Finished { .. }) {
            return Ok(());
        }
        if matches!(
            event.event,
            Event::SkillsInitialized { .. }
                | Event::PromptInitialized { .. }
                | Event::ContextUsage(_)
                | Event::CompactStarted { .. }
                | Event::CompactDone { .. }
        ) {
            return self.context_event(&id, event.event).await;
        }
        let turn = active.turn_id.clone();
        if let Event::ToolExecuting { id: call } = &event.event {
            self.legacy_tool_executing(&id, &turn, call);
            return Ok(());
        }
        if let Event::ToolBatch { ids } = event.event {
            self.legacy_tool_batch(&id, &turn, ids);
            return Ok(());
        }
        if matches!(event.event, Event::FilePrepared { .. }) {
            return self.file_checkpoint_event(&id, event.event).await;
        }
        if matches!(event.event, Event::Subagent { .. }) {
            return self.subagent_event(&id, event.event).await;
        }
        if matches!(
            event.event,
            Event::GoalStep { .. } | Event::GoalVerdict { .. }
        ) {
            return self.goal_event(&id, event.event).await;
        }
        if let Event::Todos {
            call_id,
            write,
            reply,
        } = event.event
        {
            return self.todo_tool(&id, &call_id, write, reply).await;
        }
        if matches!(
            event.event,
            Event::TodoReminder { .. } | Event::PlanMode { .. } | Event::Reminder { .. }
        ) {
            return self.plan_event(&id, event.event).await;
        }
        if let Event::Question {
            call_id,
            input,
            reply,
        } = event.event
        {
            return self
                .register_question(&id, &event.run_id, &call_id, *input, reply)
                .await;
        }
        if let Event::Permission {
            call,
            request,
            reply,
        } = event.event
        {
            if reply.is_closed() {
                return Ok(());
            }
            let host = self.register_permission(&id, &call, request, reply)?;
            if host != id {
                self.persist(&host, None).await?;
            }
            return self.persist(&id, None).await;
        }
        if let Event::StepBoundary { committed } = event.event {
            if let Some(messages) = self.drain_mailbox(&id, &turn).await? {
                let _ = committed.send(Some(crate::contract::Guide {
                    messages,
                    origin: None,
                    tool_disallowlist: vec![],
                }));
                return Ok(());
            }
            return self.drain_guide(&id, &turn, committed).await;
        }
        if let Event::RequestAuth {
            provider,
            selection,
            access,
            reply,
        } = event.event
        {
            self.request_auth(&id, &turn, provider, selection, access, reply);
            return Ok(());
        }
        if matches!(event.event, Event::Finished { .. }) {
            // run 结束：权限、问答与 Host 请求统一在此收口，不再分散清理。
            self.release_waiters(&id);
        }
        let now = self.clock.now();
        let s = self.sessions.get_mut(&id).unwrap();
        let legacy = super::legacy_stream::fact(&event.event, s.runtime.legacy.kind().is_some());
        let mut deltas = vec![];
        let mut finished = false;
        let text_only = matches!(event.event, Event::Text { .. });
        let mut receipt = None;
        match event.event {
            Event::FilePrepared { .. }
            | Event::Todos { .. }
            | Event::Subagent { .. }
            | Event::GoalStep { .. }
            | Event::GoalVerdict { .. }
            | Event::SkillsInitialized { .. }
            | Event::TodoReminder { .. }
            | Event::PlanMode { .. }
            | Event::Reminder { .. }
            | Event::Hook(_)
            | Event::PermissionHook { .. }
            | Event::PromptBlocked { .. }
            | Event::WorkspaceHooks { .. }
            | Event::Question { .. }
            | Event::ToolCleanupFailed(_)
            | Event::ToolExecuting { .. }
            | Event::ModelStatus(_)
            | Event::ToolBatch { .. }
            | Event::StepBoundary { .. }
            | Event::Permission { .. }
            | Event::Background { .. }
            | Event::PromptInitialized { .. }
            | Event::AuxiliaryDone { .. }
            | Event::StreamRecovery { .. }
            | Event::UsageDone { .. }
            | Event::RequestAuth { .. }
            | Event::ContextUsage(_)
            | Event::CompactStarted { .. }
            | Event::CompactDone { .. } => unreachable!(),
            Event::Text {
                response_id,
                text,
                reasoning,
            } => {
                let kind = if reasoning {
                    "reasoning"
                } else {
                    "assistantText"
                };
                if let Some(row) = s
                    .rows
                    .iter_mut()
                    .rev()
                    .find(|r| r["assistantResponseId"] == response_id && r["kind"] == kind)
                {
                    let serde_json::Value::String(current) = &mut row["text"] else {
                        bail!("Invalid text projection");
                    };
                    if current.len() + text.len() > crate::domain::MAX_TEXT_BYTES {
                        bail!("Projected text exceeds limit");
                    }
                    current.push_str(&text);
                    deltas.push(
                        json!({"op":"row.delta","rowId":row["rowId"],"path":"text","append":text}),
                    );
                } else {
                    let mut row = s.row(kind, &turn, &self.clock.id(), now);
                    row["assistantResponseId"] = response_id.into();
                    row["text"] = text.into();
                    row["state"] = "streaming".into();
                    s.rows.push(row.clone());
                    deltas.push(json!({"op":"row.appended","row":row}));
                }
            }
            Event::ModelDone {
                stable,
                message,
                usage,
                committed,
            } => {
                receipt = Some(committed);
                if let Some(message) = message {
                    s.append_message(message);
                }
                for row in &mut s.rows {
                    if row["turnId"] == turn && row["state"] == "streaming" {
                        row["state"] = "complete".into();
                        deltas.push(json!({"op":"row.upserted","row":row}));
                    }
                }
                if stable {
                    s.record_response(&turn);
                }
                super::goal_events::account_usage(s, &usage, now);
                if s.goal.as_ref().is_some_and(|g| g.active() && g.exhausted()) {
                    s.goal.as_mut().unwrap().pause(now);
                    // 预算耗尽的 assistant 事实先提交；取消后绝不能再执行它声明的工具。
                    self.active[&id].cancel.cancel();
                }
            }
            Event::ToolStart { call } => {
                let call_id = call["id"].as_str().unwrap();
                let mut row = s.row("toolCall", &turn, call_id, now);
                row["toolCallId"] = call["id"].clone();
                row["toolName"] = call["function"]["name"].clone();
                row["inputText"] = call["function"]["arguments"].clone();
                row["status"] = "running".into();
                row["startedAt"] = now.into();
                s.rows.push(row.clone());
                deltas.push(json!({"op":"row.appended","row":row}));
            }
            Event::ToolDone {
                id: call_id,
                result,
                model_content,
                display,
                failed,
                denied,
                committed,
            } => {
                receipt = Some(committed);
                let content = model_content.unwrap_or_else(|| result.clone().into());
                s.append_message(json!({"role":"tool","tool_call_id":call_id,"content":content,"_zcode_tool_failed":failed}));
                if let Some(row) = s
                    .rows
                    .iter_mut()
                    .find(|r| r["turnId"] == turn && r["toolCallId"] == call_id)
                {
                    // 与 Node permission_denied 一致：被拒绝的调用不产生工具错误，行收口为 cancelled。
                    row["status"] = if denied {
                        "cancelled"
                    } else if failed {
                        "error"
                    } else {
                        "success"
                    }
                    .into();
                    row["endedAt"] = now.into();
                    row["output"] = json!({"text":result});
                    if let Some(display) = display {
                        row["output"]["display"] = display;
                    }
                    row.as_object_mut().unwrap().remove("approvalInteractionId");
                    if failed && !denied {
                        row["error"] =
                            json!({"code":"fault.tool.failed","message":"Tool execution failed"});
                    }
                    deltas.push(json!({"op":"row.upserted","row":row}));
                }
            }
            Event::Finished {
                error,
                model_failure,
                cancelled,
            } => {
                // cancel 可在 loop 正常返回与 Finished 入队之间到达，以 owner 的取消事实为准。
                let cancelled = cancelled || self.active[&id].cancel.is_cancelled();
                if let Some(goal) = &mut s.goal {
                    if cancelled || error.is_some() {
                        goal.pause(now);
                    } else {
                        goal.settle(now);
                    }
                }
                super::busy_input::fallback_guides(
                    s,
                    if cancelled || error.is_some() {
                        "guide.turnInterrupted"
                    } else {
                        "guide.noToolBoundary"
                    },
                );
                finished = true;
                s.api_retry = None;
                s.run_id = None;
                // 修复：后台子代理的权限请求挂在根会话上、由子会话持有，根 run 结束不能把它们一起清掉，
                // 否则子会话仍在等待却再也无法应答。
                let waiters = &self.waiters;
                s.pending.retain(|p| {
                    p["interactionId"]
                        .as_str()
                        .and_then(|i| waiters.permission(i))
                        .is_some_and(|w| w.owner != id)
                });
                s.revision += 1;
                let outcome = if cancelled {
                    "interrupted"
                } else if error.is_some() {
                    "failed"
                } else {
                    "success"
                };
                tracing::info!(
                    target: "zcode::runtime",
                    event = "run.finished",
                    session_id = id.as_str(),
                    outcome,
                    failure = model_failure.as_ref().map(|f| f.reason),
                    "Run finished"
                );
                s.phase = match outcome {
                    "interrupted" => crate::domain::execution::Phase::CompletedInterrupted,
                    "failed" => crate::domain::execution::Phase::Error,
                    _ => crate::domain::execution::Phase::CompletedSuccess,
                };
                if cancelled || error.is_some() {
                    if s.queued_now.is_none() {
                        s.auto_drain = false;
                    }
                    s.close_unfinished_tools();
                }
                if !cancelled && let Some(message) = error {
                    s.last_error = Some(
                        json!({"code":"fault.runtime.execution","message":message,"recoverable":true,"at":now,"source":"runtime"}),
                    );
                    if let Some(failure) = model_failure {
                        let error = failure.last_error((&s.provider, &s.model), now);
                        s.last_error = Some(error);
                    }
                }
                s.finish_rows(outcome, now);
                deltas.extend(
                    s.rows
                        .iter()
                        .filter(|r| r["turnId"] == turn)
                        .map(|r| json!({"op":"row.upserted","row":r})),
                );
                self.active.remove(&id);
            }
        }
        s.updated_at = now;
        let checkpoint = !text_only || now.saturating_sub(s.checkpoint_at) >= 250;
        if checkpoint {
            s.checkpoint_at = now;
        }
        if finished {
            deltas.extend(
                super::file_changes::hydrate(
                    self.sessions.get_mut(&id).unwrap(),
                    self.tools.as_ref(),
                    Some(&turn),
                )
                .await?,
            );
        }
        self.publish(&id, deltas)?;
        self.legacy_fact(&id, &turn, legacy)?;
        if checkpoint {
            self.persist(&id, None).await?;
        }
        if let Some(receipt) = receipt {
            let _ = receipt.send(());
        }
        if finished {
            self.promote(&id).await?;
            self.deliver_children(&id).await?;
            self.finish_child(&id).await?;
        }
        Ok(())
    }
}
