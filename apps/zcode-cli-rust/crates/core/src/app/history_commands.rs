use super::Engine;
use crate::{
    contract::StorageCommitFailure,
    domain::{
        history::{InputBoundary, ResponseBoundary},
        protocol::Command,
    },
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn history_command(&mut self, c: &Command) -> Result<Value> {
        let id = c.session_id.as_deref().context("Session required")?;
        let s = &self.sessions[id];
        ensure!(
            c.base_revision.is_some() && c.base_log_epoch.is_some(),
            "History action requires revision and epoch"
        );
        let target = &c.payload["target"];
        let found = target.as_object().filter(|v| v.len() == 2).and_then(|_| {
            s.rows
                .iter()
                .position(|r| r["rowId"] == target["rowId"] && r["entityId"] == target["entityId"])
        });
        let Some(index) = found else {
            return Ok(c.ack("rejected", s.revision, Some("guard.targetNotFound")));
        };
        if c.kind == "forkAssistant" {
            let boundary = s
                .history
                .responses
                .iter()
                .find(|b| b.row == index && s.rows[index]["state"] == "complete")
                .cloned();
            return match boundary {
                Some(b) if self.journaled(id) => self.node_fork(c, b).await,
                Some(b) => self.fork_history(c, b).await,
                None => Ok(c.ack("rejected", s.revision, Some("guard.forkTargetNotStable"))),
            };
        }
        let latest = s
            .rows
            .iter()
            .rposition(|r| r["kind"] == "userInput" && r["origin"] == "realUser");
        let boundary = if c.kind == "editUserQuery" {
            s.history
                .inputs
                .iter()
                .find(|b| Some(index) == latest && s.rows[index]["entityId"] == b.entity)
        } else {
            let turn = s
                .rows
                .iter()
                .rev()
                .find(|r| r["kind"] == "turnHeader")
                .map(|r| &r["turnId"]);
            s.history.inputs.iter().rev().find(|b| {
                s.rows[index]["kind"] == "assistantText"
                    && s.rows.iter().rposition(|r| r["kind"] == "assistantText") == Some(index)
                    && turn == Some(&s.rows[index]["turnId"])
                    && s.rows[index]["turnId"] == b.turn
            })
        }
        .cloned();
        // Node 存储下截断以存储的 user 消息为锚点；找不到锚点时不能只改内存历史。
        let boundary = boundary.filter(|b| !self.journaled(id) || b.node_message.is_some());
        let Some(boundary) = boundary else {
            return Ok(c.ack(
                "rejected",
                s.revision,
                Some(if c.kind == "editUserQuery" {
                    "guard.latestQueryEditOnly"
                } else {
                    "guard.latestAssistantRetryOnly"
                }),
            ));
        };
        let mut replay = c.clone();
        replay.kind = if boundary.kind == "sendGoalCommand" {
            "sendGoalCommand"
        } else {
            "sendText"
        }
        .into();
        replay.payload = boundary.payload.clone();
        if c.kind == "editUserQuery" {
            replay.payload["text"] = c.payload["newText"]
                .as_str()
                .context("newText required")?
                .into();
            replay
                .payload
                .as_object_mut()
                .unwrap()
                .remove("displayText");
            if let Some(attachments) = c.payload.get("attachments") {
                replay.payload["attachments"] = attachments.clone();
            }
            ensure!(
                matches!(
                    c.payload["workspaceMode"].as_str(),
                    None | Some("preserve" | "rewind")
                ),
                "Workspace rewind requires file checkpoint transaction"
            );
        }
        self.validate_input(&replay.payload)?;
        if replay.kind == "sendGoalCommand" {
            super::goal_commands::validate_objective(replay.payload["text"].as_str().unwrap())?;
        }
        let selected = self.select(&replay.payload, Some(self.session_selection(id)?))?;
        let assets = self
            .prepare_attachments(id, &mut replay.payload, &selected)
            .await?;
        if c.payload["workspaceMode"] == "rewind" {
            let preview = self
                .tools
                .rewind_preview(&self.rewind_changes(id, &c.payload["target"])?)
                .await?;
            if preview["canApply"] != true
                || preview["ignoredFiles"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
            {
                let mut ack = c.ack("accepted", self.sessions[id].revision, None);
                ack["result"] = json!({"type":"editUserQuery","disposition":"blocked","sessionId":id,"reasonCode":"workspaceRewindUnsafe","preview":preview});
                self.persist(id, Some((c.key(), ack.clone()))).await?;
                self.acks.insert(c.key(), ack.clone());
                return Ok(ack);
            }
        }
        self.quiesce_history(id).await?;
        self.sessions
            .get_mut(id)
            .unwrap()
            .attachments
            .extend(assets);
        self.rerun_history(c, replay, boundary).await
    }
    pub(super) async fn quiesce_history(&mut self, id: &str) -> Result<()> {
        let s = self.sessions.get_mut(id).unwrap();
        s.auto_drain = false;
        s.queued_now = None;
        if let Some(goal) = &mut s.goal {
            goal.pause(self.clock.now());
        }
        if let Some(active) = self.active.get(id) {
            active.cancel.cancel();
        }
        self.cancel_auth(id);
        self.cancel_children(id).await?;
        self.tools.cancel_session(id, None).await?;
        while self.active.contains_key(id)
            || self.sessions[id].children.values().any(|t| t.running())
            || self.sessions[id]
                .background
                .values()
                .any(|t| t.status == "running")
        {
            let event = self.event_rx.recv().await.context("Run channel closed")?;
            self.apply_event(event).await?;
        }
        Ok(())
    }
    async fn rerun_history(
        &mut self,
        c: &Command,
        mut replay: Command,
        b: InputBoundary,
    ) -> Result<Value> {
        let id = c.session_id.as_deref().unwrap();
        let transaction = if c.payload["workspaceMode"] == "rewind" {
            Some(self.prepare_rewind(c).await?)
        } else {
            None
        };
        if let Some(tx) = &transaction {
            self.mark_rewind(id, &c.command_id, tx.as_ref());
        }
        let now = self.clock.now();
        let s = self.sessions.get_mut(id).unwrap();
        if let Some(provenance) = rerun_provenance(s, &b) {
            replay.payload["_provenance"] = provenance;
        }
        if let Some(target) = &b.node_message {
            // Node 重试以被重试的 assistant 为请求锚点，编辑以 user 消息本身为锚点。
            let retried = s.history.responses.iter().find(|r| r.turn == b.turn);
            let anchor = retried
                .and_then(|r| r.node_message.clone())
                .filter(|_| c.kind == "retryTurn")
                .unwrap_or_else(|| target.clone());
            s.node_rewind(now, target, &anchor);
        }
        s.cut_history(b.row, b.message, &b.state);
        s.epoch = self.clock.id();
        s.seq = 0;
        s.revision += 1;
        // 重跑不自动执行已有排队输入；保留队列由用户按原协议恢复。
        replay.payload["_historyRerun"] = true.into();
        let intent = self.node_admit_now(id, &replay, &c.kind);
        let (turn, _) = self.admit_input(id, &replay, None)?;
        self.node_prompt(id, &turn, &replay, (intent, None));
        self.sessions.get_mut(id).unwrap().history_actions();
        let mut ack = c.ack("accepted", self.sessions[id].revision, None);
        if c.kind == "editUserQuery" {
            ack["result"] = json!({"type":"editUserQuery","disposition":"rewind","sessionId":id});
        }
        if let Err(error) = self.persist(id, Some((c.key(), ack.clone()))).await {
            if let Some(tx) = transaction {
                let _ = tx.finish(false).await;
            }
            return Err(error);
        }
        if let Some(tx) = transaction {
            tx.finish(true).await.context(StorageCommitFailure)?;
        }
        self.acks.insert(c.key(), ack.clone());
        self.history_snapshot(id)?;
        self.start_run(id, turn)?;
        Ok(ack)
    }
    async fn fork_history(&mut self, c: &Command, b: ResponseBoundary) -> Result<Value> {
        let parent = c.session_id.as_deref().unwrap();
        let mut child = self.sessions[parent].clone();
        child.cut_history(b.row + 1, b.message, &b.state);
        child.file_checkpoints.retain(|c| {
            c.row
                <= child
                    .rows
                    .last()
                    .and_then(|r| r["rowId"].as_u64())
                    .unwrap_or(0)
        });
        child.rewind_committed = None;
        child.id = self.clock.id();
        child.parent_id = Some(parent.into());
        child.task_type = "interactive".into();
        child.agent_profile = None;
        child.created_at = self.clock.now();
        child.updated_at = child.created_at;
        child.epoch = self.clock.id();
        child.seq = 0;
        child.revision = 1;
        child.phase = crate::domain::execution::Phase::CompletedSuccess;
        child.auto_drain = true;
        child.queued_now = None;
        child.queue.clear();
        child.children.clear();
        child.background.clear();
        child.pending_acks.clear();
        child.creation_ack = None;
        child.listed = true;
        child.archived = false;
        child.archived_at = None;
        if let Some(goal) = &mut child.goal {
            goal.pause(self.clock.now());
        }
        child.finish_rows("success", self.clock.now());
        child.history_actions();
        let id = child.id.clone();
        let mut ack = c.ack("accepted", self.sessions[parent].revision, None);
        ack["result"] = json!({"type":"forkAssistant","sessionId":id});
        // 新 session 与父命令的幂等 ACK 在同一事务中提交；故障不得留下可重复创建的分支。
        self.store
            .commit_receipt(
                &self.workspace,
                Some(&mut child),
                Some((c.key(), ack.clone())),
            )
            .await
            .context(StorageCommitFailure)?;
        self.durable_acks.insert(c.key());
        self.acks.insert(c.key(), ack.clone());
        self.tools.inherit_session(parent, &id).await?;
        let summary = child.summary();
        self.sessions.insert(id.clone(), child);
        self.publish_index(&id, Some(summary))?;
        Ok(ack)
    }
    pub(super) fn history_snapshot(&mut self, id: &str) -> Result<()> {
        self.reset_topic(id)
    }
}

/// Node `inputIntentMetadataFromCanonical` provenance: the rerun keeps the
/// original input's own provenance, else points at the original command.
fn rerun_provenance(s: &crate::domain::session::Session, b: &InputBoundary) -> Option<Value> {
    if let Some(provenance) = b.payload.get("_provenance").filter(|p| p.is_object()) {
        return Some(provenance.clone());
    }
    let row = &s.rows[b.user_row];
    let command = row["sourceCommandId"].as_str().filter(|c| !c.is_empty())?;
    let mut provenance =
        json!({"sourceCommandId": command, "queueItemId": format!("queue_{command}")});
    if let Some(client) = row["clientId"].as_str().filter(|c| !c.is_empty()) {
        provenance["clientId"] = client.into();
    }
    Some(provenance)
}
