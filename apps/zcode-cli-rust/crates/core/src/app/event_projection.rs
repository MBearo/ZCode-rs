use super::{Engine, Event, RunEvent};
use anyhow::{Result, bail};
use serde_json::json;
impl Engine {
    pub(super) async fn apply_event(&mut self, event: RunEvent) -> Result<()> {
        if self.auxiliary.contains_key(&event.session_id) {
            return self.auxiliary_event(event);
        }
        if let Event::ToolCleanupFailed(message) = event.event {
            let owned = self
                .active
                .get(&event.session_id)
                .is_some_and(|a| a.run_id == event.run_id)
                || self.sessions.get(&event.session_id).is_some_and(|s| {
                    s.background
                        .values()
                        .any(|task| task.run_id == event.run_id && task.status == "running")
                });
            if owned {
                bail!("{message}");
            }
            return Ok(());
        }
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
        if let Event::TodoReminder { reply } = event.event {
            return self.todo_reminder(&id, reply).await;
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
        if let Event::StepBoundary { committed } = event.event {
            if let Some(messages) = self.drain_mailbox(&id, &turn).await? {
                let _ = committed.send(Some(messages));
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
            if !reply.is_closed() {
                let request_id = format!("rust-auth-{}", self.clock.id());
                let workspace = json!({"workspaceKey":self.workspace,"workspacePath":self.workspace_path,"workspaceIdentity":self.workspace});
                let params = json!({"requestId":request_id,"sessionId":id,"turnId":turn,"workspace":workspace,"providerId":provider,"modelSelection":selection,"accountAccess":access,"reason":"model-request"});
                self.auth.insert(
                    request_id.clone(),
                    (id.clone(), event.run_id, workspace, reply),
                );
                self.outbox.push(json!({"id":request_id,"method":"interaction/requestProviderRuntimeHeaders","params":params}));
            }
            return Ok(());
        }
        if matches!(event.event, Event::Finished { .. }) {
            self.cancel_auth(&id);
        }
        let now = self.clock.now();
        let s = self.sessions.get_mut(&id).unwrap();
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
            | Event::Question { .. }
            | Event::ToolCleanupFailed(_)
            | Event::StepBoundary { .. }
            | Event::Background { .. }
            | Event::PromptInitialized { .. }
            | Event::AuxiliaryDone { .. }
            | Event::RequestAuth { .. }
            | Event::ContextUsage(_)
            | Event::CompactStarted { .. }
            | Event::CompactDone { .. } => unreachable!(),
            Event::Retry(state) => {
                s.api_retry = state;
            }
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
            Event::Permission { call, reply } => {
                let interaction = self.clock.id();
                let row = s
                    .rows
                    .iter_mut()
                    .find(|r| r["turnId"] == turn && r["toolCallId"] == call["id"])
                    .unwrap();
                row["status"] = "pendingApproval".into();
                row["approvalInteractionId"] = interaction.clone().into();
                s.pending.push(json!({"interactionId":interaction,"kind":"permission","anchorRowId":row["rowId"],"createdAt":now,
                    "payload":{"kind":"permission","toolCallId":call["id"],"toolName":call["function"]["name"],"summary":format!("Allow {}?",call["function"]["name"].as_str().unwrap()),"detail":call["function"]["arguments"],"options":[{"optionId":"allowOnce","label":"Allow once","kind":"allowOnce"},{"optionId":"deny","label":"Deny","kind":"deny"}]}}));
                deltas.push(json!({"op":"row.upserted","row":row}));
                self.permissions.insert(interaction, (id.clone(), reply));
            }
            Event::ToolDone {
                id: call_id,
                result,
                display,
                failed,
                committed,
            } => {
                receipt = Some(committed);
                s.append_message(json!({"role":"tool","tool_call_id":call_id,"content":result,"_zcode_tool_failed":failed}));
                if let Some(row) = s
                    .rows
                    .iter_mut()
                    .find(|r| r["turnId"] == turn && r["toolCallId"] == call_id)
                {
                    row["status"] = if failed { "error" } else { "success" }.into();
                    row["endedAt"] = now.into();
                    row["output"] = json!({"text":result});
                    if let Some(display) = display {
                        row["output"]["display"] = display;
                    }
                    row.as_object_mut().unwrap().remove("approvalInteractionId");
                    if failed {
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
                s.pending.clear();
                s.revision += 1;
                let outcome = if cancelled {
                    "interrupted"
                } else if error.is_some() {
                    "failed"
                } else {
                    "success"
                };
                s.phase = match outcome {
                    "interrupted" => "completedInterrupted",
                    "failed" => "error",
                    _ => "completedSuccess",
                }
                .into();
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
                        s.last_error = Some(json!({"code":failure.code,"message":failure.message,
                            "recoverable":failure.retryable,"at":now,"source":"provider",
                            "attribution":{"source":"provider","reason":failure.reason,"providerId":s.provider,"modelId":s.model,"retryable":failure.retryable}}));
                        if let Some(status) = failure.status_code {
                            s.last_error.as_mut().unwrap()["attribution"]["statusCode"] =
                                status.into();
                        }
                    }
                }
                s.finish_rows(outcome, now);
                deltas.extend(
                    s.rows
                        .iter()
                        .filter(|r| r["turnId"] == turn)
                        .map(|r| json!({"op":"row.upserted","row":r})),
                );
                self.permissions.retain(|_, (session, _)| session != &id);
                self.questions.retain(|_, q| q.session != id);
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
