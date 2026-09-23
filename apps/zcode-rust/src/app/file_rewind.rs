use super::Engine;
use crate::{
    contract::{Event, RewindTransaction},
    domain::{file_checkpoint::FileCheckpoint, protocol::Command},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
impl Engine {
    pub(super) async fn file_checkpoint_event(&mut self, id: &str, event: Event) -> Result<()> {
        let Event::FilePrepared {
            mut change,
            committed,
        } = event
        else {
            unreachable!()
        };
        let s = &self.sessions[id];
        change.row = s
            .rows
            .iter()
            .rev()
            .find(|r| {
                r["kind"] == "toolCall" && r["status"] == "running" && r["toolName"] == change.tool
            })
            .and_then(|r| r["rowId"].as_u64())
            .context("Checkpoint has no active tool")?;
        let mut owner = id.to_owned();
        let mut child = id.to_owned();
        loop {
            let s = self
                .sessions
                .get_mut(&owner)
                .context("Checkpoint owner unavailable")?;
            if owner != id {
                let task = s
                    .children
                    .values()
                    .find(|t| t.child_id == child)
                    .context("Checkpoint child not owned")?;
                change.row = s
                    .rows
                    .iter()
                    .rev()
                    .find(|r| r["toolCallId"] == task.call_id)
                    .and_then(|r| r["rowId"].as_u64())
                    .context("Checkpoint parent anchor unavailable")?;
            }
            s.file_checkpoints.push(change.clone());
            let next = s.agent_profile.as_ref().and(s.parent_id.clone());
            self.persist(&owner, None).await?;
            match next {
                Some(parent) => {
                    child = owner;
                    owner = parent;
                }
                None => break,
            }
        }
        let _ = committed.send(());
        Ok(())
    }
    pub(super) fn rewind_changes(&self, id: &str, target: &Value) -> Result<Vec<FileCheckpoint>> {
        let s = &self.sessions[id];
        let row = s
            .rows
            .iter()
            .find(|r| r["rowId"] == target["rowId"] && r["entityId"] == target["entityId"])
            .context("Rewind target unavailable")?;
        // assistant/工具卡代表所在用户轮；文件撤销包含该轮及其后的工具修改。
        let anchor = s
            .rows
            .iter()
            .find(|r| r["turnId"] == row["turnId"])
            .and_then(|r| r["rowId"].as_u64())
            .context("Rewind turn unavailable")?;
        Ok(s.file_checkpoints
            .iter()
            .filter(|c| c.row >= anchor && !c.restored)
            .cloned()
            .collect())
    }
    pub(super) async fn rewind_preview(&mut self, p: &Value) -> Result<Value> {
        ensure!(
            p.as_object().is_some_and(|o| o.len() == 4),
            "Invalid file rewind preview parameters"
        );
        let id = p["sessionId"].as_str().context("Session required")?;
        self.ensure_session(id).await?;
        let s = &self.sessions[id];
        ensure!(
            p["baseLogEpoch"] == s.epoch && p["baseRevision"] == s.revision,
            "proto.staleRevision"
        );
        self.tools
            .rewind_preview(&self.rewind_changes(id, &p["target"])?)
            .await
    }
    pub(super) async fn prepare_rewind(
        &mut self,
        c: &Command,
    ) -> Result<Box<dyn RewindTransaction>> {
        let id = c.session_id.as_deref().unwrap();
        let changes = self.rewind_changes(id, &c.payload["target"])?;
        self.tools.begin_rewind(id, &c.command_id, &changes).await
    }
    pub(super) fn mark_rewind(
        &mut self,
        id: &str,
        token: &str,
        transaction: &dyn RewindTransaction,
    ) {
        let ids = transaction.checkpoint_ids();
        let s = self.sessions.get_mut(id).unwrap();
        for c in &mut s.file_checkpoints {
            if ids.contains(&c.id) {
                c.restored = true;
            }
        }
        s.rewind_committed = Some(token.into());
    }
    pub(super) async fn apply_file_rewind(&mut self, c: &Command) -> Result<Value> {
        let id = c.session_id.as_deref().context("Session required")?;
        ensure!(
            c.base_revision.is_some() && c.base_log_epoch.is_some(),
            "File rewind requires revision and epoch"
        );
        let preview = self
            .tools
            .rewind_preview(&self.rewind_changes(id, &c.payload["target"])?)
            .await?;
        if preview["canApply"] != true {
            let mut ack = c.ack("accepted", self.sessions[id].revision, None);
            ack["result"] = json!({"type":"applyFileRewind","applied":false,"preview":preview,"response":"No safe file changes to restore."});
            self.persist(id, Some((c.key(), ack.clone()))).await?;
            self.acks.insert(c.key(), ack.clone());
            return Ok(ack);
        }
        self.quiesce_history(id).await?;
        let transaction = self.prepare_rewind(c).await?;
        let preview = transaction.preview();
        self.mark_rewind(id, &c.command_id, transaction.as_ref());
        let s = self.sessions.get_mut(id).unwrap();
        let turn = s
            .rows
            .last()
            .and_then(|r| r["turnId"].as_str())
            .context("Turn missing")?
            .to_owned();
        let mut row = s.row("timelineMarker", &turn, &self.clock.id(), self.clock.now());
        row["marker"] = json!({"type":"checkpointRestored","checkpointId":c.command_id});
        s.rows.push(row.clone());
        s.revision += 1;
        let mut ack = c.ack("accepted", s.revision, None);
        ack["result"] = json!({"type":"applyFileRewind","applied":true,"preview":preview,"response":"Restored tracked files from byte checkpoints."});
        let mut deltas = super::file_changes::restored(s);
        deltas.push(json!({"op":"row.appended","row":row}));
        self.publish(id, deltas)?;
        if let Err(error) = self.persist(id, Some((c.key(), ack.clone()))).await {
            let _ = transaction.finish(false).await;
            return Err(error);
        }
        transaction
            .finish(true)
            .await
            .context(crate::contract::StorageCommitFailure)?;
        self.acks.insert(c.key(), ack.clone());
        Ok(ack)
    }
}
