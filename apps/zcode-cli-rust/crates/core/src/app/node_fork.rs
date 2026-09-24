//! `forkAssistant` over Node storage (spec rust-m11-node-storage §5.2): the
//! child is committed by Node's atomic fork bundle at the stored stable
//! boundary, then registered from its stored records like any cold session.
use super::Engine;
use crate::contract::StorageCommitFailure;
use crate::domain::{
    history::ResponseBoundary, node_journal::Op, protocol::Command, session::Session,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn node_fork(&mut self, c: &Command, b: ResponseBoundary) -> Result<Value> {
        let parent = c.session_id.as_deref().context("Session required")?;
        let s = &self.sessions[parent];
        let Some(boundary) = b.node_message.clone() else {
            return Ok(c.ack("rejected", s.revision, Some("guard.forkTargetNotStable")));
        };
        let now = self.clock.now();
        let child = self.new_session_id();
        let request = json!({"parent": parent, "boundary": boundary, "command": c.command_id,
            "revision": c.base_revision.unwrap_or(0), "selection": s.node_selection(),
            "execution": {"mode": s.mode.as_str(), "planEnabled": s.plan_enabled}});
        // 子会话由 Fork 写入在同一事务中创建；载体只承载这批写入。
        let mut carrier = Session::new(
            child.clone(),
            self.workspace.clone(),
            String::new(),
            String::new(),
            String::new(),
            self.clock.id(),
            now,
        );
        carrier.node.created = true;
        carrier.node.push(now, Op::Fork(request));
        let mut ack = c.ack("accepted", s.revision, None);
        ack["result"] = json!({"type": "forkAssistant", "sessionId": child});
        self.store
            .commit_receipt(
                &self.workspace,
                Some(&mut carrier),
                Some((c.key(), ack.clone())),
            )
            .await
            .context(StorageCommitFailure)?;
        self.durable_acks.insert(c.key());
        self.acks.insert(c.key(), ack.clone());
        self.ensure_session(&child).await?;
        self.tools.inherit_session(parent, &child).await?;
        Ok(ack)
    }
}
