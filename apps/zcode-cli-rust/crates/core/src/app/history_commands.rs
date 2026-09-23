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
        let s = self.sessions.get_mut(id).unwrap();
        s.cut_history(b.row, b.message, &b.state);
        s.epoch = self.clock.id();
        s.seq = 0;
        s.revision += 1;
        // 重跑不自动执行已有排队输入；保留队列由用户按原协议恢复。
        replay.payload["_historyRerun"] = true.into();
        let (turn, _) = self.admit_input(id, &replay, None)?;
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
        child.phase = "completedSuccess".into();
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
        let topic = format!("conversation/{id}");
        let subs = self
            .subscriptions
            .values()
            .filter(|s| s.topic == topic && !s.paused)
            .map(|s| s.id.clone())
            .collect::<Vec<_>>();
        for sub in subs {
            self.snapshot_frame(&sub, "recovery")?;
            self.subscriptions.get_mut(&sub).unwrap().needs_resync = false;
        }
        self.publish_index(id, Some(self.sessions[id].summary()))
    }
}
