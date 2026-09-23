use super::Engine;
use crate::domain::{MAX_TOOL_BYTES, protocol::Command, session::Session};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

impl Engine {
    /// Dispatch one command through the same admission path used by the App
    /// Server. Frontends never mutate the actor directly.
    pub async fn dispatch_command(&mut self, command: Command) -> Result<Value> {
        self.command(command).await
    }

    pub(super) async fn command(&mut self, mut c: Command) -> Result<Value> {
        if c.command_id.is_empty() || c.client_id.is_empty() || !c.issued_at.is_finite() {
            bail!("Invalid command identity");
        }
        let key = c.key();
        if let Some(mut ack) = self.cached_ack(&key).await? {
            if ack["status"] == "accepted" {
                ack["status"] = "duplicate".into();
            }
            return Ok(ack);
        }
        if matches!(
            c.kind.as_str(),
            "resolveInteraction" | "snoozeInteractionAutoResolution"
        ) {
            return self.interaction_command(&c).await;
        }
        if c.kind == "createSession" {
            return self.create(c).await;
        }
        let id = c
            .session_id
            .as_deref()
            .context("Session id required")?
            .to_owned();
        self.ensure_session(&id).await?;
        let s = self.sessions.get(&id).context("Session unavailable")?;
        if matches!(
            c.kind.as_str(),
            "editQueueItem" | "deleteQueueItem" | "reorderQueueItem" | "setAutoDrain"
        ) && c.base_revision.is_none()
        {
            bail!("baseRevision required for queue mutation");
        }
        if c.base_log_epoch
            .as_ref()
            .is_some_and(|epoch| *epoch != s.epoch)
            || c.base_revision
                .is_some_and(|revision| revision != s.revision)
        {
            return Ok(c.ack("stale", s.revision, Some("proto.staleRevision")));
        }
        if c.kind == "sendText"
            && let Some(text) = c.payload["text"].as_str()
        {
            let trimmed = text.trim();
            if trimmed == "/compact" || trimmed.starts_with("/compact ") {
                self.validate_input(&c.payload)?;
                let instructions = trimmed.strip_prefix("/compact").unwrap().trim().to_owned();
                c.kind = "compact".into();
                c.payload = json!({"text":instructions});
            }
        }
        if matches!(
            c.kind.as_str(),
            "forkAssistant" | "retryTurn" | "editUserQuery"
        ) {
            return self.history_command(&c).await;
        }
        if c.kind == "applyFileRewind" {
            return self.apply_file_rewind(&c).await;
        }
        if c.kind == "compact" {
            return self.compact_command(&c).await;
        }
        if matches!(c.kind.as_str(), "pauseGoal" | "resumeGoal") {
            return self.goal_command(&c).await;
        }
        if c.kind == "sendGoalCommand" {
            let text = c.payload["text"]
                .as_str()
                .context("Goal objective required")?
                .trim()
                .to_owned();
            super::goal_commands::validate_objective(&text)?;
            c.payload["text"] = text.into();
            c.payload["requestedDelivery"] = "queue".into();
            return self.send_input(c).await;
        }
        if c.kind == "sendQueuedNow" {
            return self.send_queued_now(&c).await;
        }
        if c.kind == "deleteSession" {
            return self.close_session(&c).await;
        }
        if c.kind == "sendText" {
            return self.send_input(c).await;
        }
        let s = self.sessions.get(&id).unwrap();
        let mut ack = c.ack("accepted", s.revision, None);
        let mut deltas = vec![];
        match c.kind.as_str() {
            "discardSharedContext" => {
                anyhow::ensure!(
                    c.payload.as_object().is_some_and(|p| p.len() == 1),
                    "Invalid discard payload"
                );
                let context_id = c.payload["contextId"]
                    .as_str()
                    .filter(|id| !id.trim().is_empty())
                    .context("Context ID required")?
                    .trim();
                let context = self.sessions.get_mut(&id).unwrap().shared_context.as_mut();
                let Some(context) = context.filter(|ctx| {
                    ctx.provenance.context_id.as_deref() == Some(context_id)
                        && ctx.provenance.status == crate::domain::shared_context::Status::Pending
                }) else {
                    return Ok(c.ack(
                        "rejected",
                        self.sessions[&id].revision,
                        Some("fault.command.inputRejected"),
                    ));
                };
                context.provenance.status = crate::domain::shared_context::Status::Discarded;
                self.sessions.get_mut(&id).unwrap().revision += 1;
            }
            "switchModelConfig" => {
                let selected = self.select(&c.payload, Some(self.session_selection(&id)?))?;
                if selected == self.session_selection(&id)? {
                    return Ok(c.ack("noop", s.revision, Some("config.unchanged")));
                }
                if let Some(row) = self.selection_marker(&id, &selected, &c.command_id) {
                    deltas.push(json!({"op":"row.appended","row":row}));
                }
                self.apply_selection(&id, selected)?;
                self.sessions.get_mut(&id).unwrap().revision += 1;
            }
            "switchCollaborationMode" => {
                if c.payload["mode"] != "yolo" || s.running() {
                    return Ok(c.ack("rejected", s.revision, Some("guard.capabilityUnsupported")));
                }
                let session = self.sessions.get_mut(&id).unwrap();
                session.mode = "yolo".into();
                session.revision += 1;
            }
            "cancelBackgroundWork" => {
                let task = c.payload["workId"].as_str().context("workId required")?;
                if let Some(child) = s
                    .children
                    .get(task)
                    .filter(|t| t.running())
                    .map(|t| t.child_id.clone())
                {
                    self.cancel_children(&child).await?;
                    if let Some(active) = self.active.get(&child) {
                        active.cancel.cancel();
                    }
                    self.tools.cancel_session(&child, None).await?;
                    let ack = c.ack("accepted", self.sessions[&id].revision, None);
                    self.persist(&id, Some((key.clone(), ack.clone()))).await?;
                    self.acks.insert(key, ack.clone());
                    return Ok(ack);
                }
                if s.background.get(task).is_none_or(|t| t.status != "running") {
                    return Ok(c.ack("noop", s.revision, Some("proto.alreadyResolved")));
                }
                self.tools.cancel_session(&id, Some(task)).await?;
                self.sessions.get_mut(&id).unwrap().revision += 1;
            }
            "stop" => {
                if let Some(active) = self.active.get(&id) {
                    if c.payload["expectedForegroundExecutionId"]
                        .as_str()
                        .is_some_and(|expected| expected != active.run_id)
                    {
                        return Ok(c.ack(
                            "noop",
                            self.sessions[&id].revision,
                            Some("proto.staleExecution"),
                        ));
                    }
                    active.cancel.cancel();
                }
                self.cancel_auth(&id);
                self.cancel_children(&id).await?;
                self.tools.cancel_session(&id, None).await?;
                let s = self.sessions.get_mut(&id).unwrap();
                if let Some(goal) = &mut s.goal {
                    goal.pause(self.clock.now());
                }
                s.auto_drain = false;
                s.queued_now = None;
                for item in &mut s.queue {
                    item["dispatch"] = json!({"state":"queued"});
                    if item["delivery"]["admitted"] == "startNow" {
                        item["delivery"]["admitted"] = "queue".into();
                    }
                }
                s.api_retry = None;
                s.revision += 1;
            }
            "renameSession" => {
                let title = c.payload["title"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty() && s.len() <= 1024)
                    .context("Invalid title")?;
                let s = self.sessions.get_mut(&id).unwrap();
                s.title = title.into();
                s.title_source = "custom".into();
                s.revision += 1;
            }
            "setFollowupMode" => {
                let mode = c.payload["mode"]
                    .as_str()
                    .filter(|mode| matches!(*mode, "queue" | "guide"))
                    .context("Invalid followup mode")?;
                let session = self.sessions.get_mut(&id).unwrap();
                session.followup_mode = mode.into();
                session.revision += 1;
            }
            "setAutoDrain" => {
                let enabled = c.payload["autoDrain"]
                    .as_bool()
                    .context("autoDrain must be boolean")?;
                let s = self.sessions.get_mut(&id).unwrap();
                s.auto_drain = enabled;
                s.revision += 1;
            }
            "editQueueItem" | "deleteQueueItem" | "reorderQueueItem" => {
                let item = c.payload["queueItemId"]
                    .as_str()
                    .context("Queue id required")?;
                let s = self.sessions.get_mut(&id).unwrap();
                let pos = s
                    .queue
                    .iter()
                    .position(|q| q["queueItemId"] == item)
                    .context("Queue item unavailable")?;
                if s.queue[pos]["dispatch"]["state"] != "queued" {
                    return Ok(c.ack("rejected", s.revision, Some("guard.queueItemReserved")));
                }
                if c.kind == "editQueueItem" && s.queue[pos]["kind"] == "compact" {
                    return Ok(c.ack("rejected", s.revision, Some("guard.queueItemNotEditable")));
                }
                if c.kind == "editQueueItem" {
                    let text = c.payload["newText"]
                        .as_str()
                        .filter(|s| !s.trim().is_empty() && s.len() <= MAX_TOOL_BYTES)
                        .context("Invalid text")?;
                    if s.queue[pos]["kind"] == "sendGoalCommand" {
                        super::goal_commands::validate_objective(text)?;
                    }
                    s.queue[pos]["text"] = text.into();
                } else if c.kind == "deleteQueueItem" {
                    let removed = s.queue.remove(pos);
                    super::shared_context::release(s, &removed);
                    let removed_key = serde_json::to_string(&(
                        Some(&id),
                        removed["sourceCommandId"].as_str().unwrap(),
                    ))?;
                    if let Some(prior) = self.acks.get_mut(&removed_key) {
                        prior["status"] = "failed".into();
                        prior["reasonCode"] = "guard.queueDeleted".into();
                        prior["result"] = json!({"type":"inputDisposition","delivery":prior["result"]["delivery"]});
                        s.pending_acks.insert(removed_key, prior.clone());
                    }
                } else {
                    let before = c.payload["beforeQueueItemId"].as_str();
                    if before == Some(item) {
                        return Ok(c.ack("noop", s.revision, Some("proto.noChange")));
                    }
                    if before
                        .is_some_and(|target| !s.queue.iter().any(|q| q["queueItemId"] == target))
                    {
                        bail!("Queue target unavailable");
                    }
                    let item = s.queue.remove(pos);
                    let to = before
                        .and_then(|target| s.queue.iter().position(|q| q["queueItemId"] == target))
                        .unwrap_or(s.queue.len());
                    s.queue.insert(to, item);
                }
                s.revision += 1;
            }
            _ => return Ok(c.ack("rejected", s.revision, Some("guard.capabilityUnsupported"))),
        }
        ack["revisionAtDecision"] = self.sessions[&id].revision.into();
        self.publish(&id, deltas)?;
        self.persist(&id, Some((key.clone(), ack.clone()))).await?;
        self.acks.insert(key, ack.clone());
        if c.kind == "switchModelConfig" {
            self.notify_selection(&id)?;
        }
        if c.kind == "setAutoDrain" {
            self.promote(&id).await?;
        }
        Ok(ack)
    }
}

pub(super) fn queue_item(c: &Command, s: &Session, now: u64) -> Value {
    let mut item = json!({"sourceCommandId":c.command_id,"queueItemId":format!("queue_{}",c.command_id),"clientId":c.client_id,"kind":c.kind,"text":c.payload["text"],"attachments":c.payload.get("attachments").cloned().unwrap_or_else(||json!([])),
        "modelSelection":c.payload.get("modelSelection").cloned().unwrap_or_else(||json!({"providerId":s.provider,"modelId":s.model,"options":{"reasoningLevel":s.reasoning_level}})),"mode":s.mode,"planEnabled":false,
        "delivery":{"requested":c.payload["requestedDelivery"].as_str().unwrap_or("auto"),"admitted":"queue"},"order":{"admissionSeq":s.revision+1,"queuePosition":s.queue.len()},
        "steer":{"state":"notRequested"},"dispatch":{"state":"queued"},"admittedAt":now});
    if let Some(refs) = c.payload.get("context_refs") {
        item["sharedContextRefs"] = refs.clone();
    }
    item
}
