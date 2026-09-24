//! V4 `createSelectionSideSession` (Node `selection-side-session.ts` and
//! `createSelectionSideConversation`, spec rust-m11-node-storage §5.2): the
//! side chat is committed by the fork bundle from the parent's stored active
//! transcript, registered from its stored records, and its first input is
//! admitted on the child like any `sendText`, leaving the parent's queue and
//! run untouched.
use super::Engine;
use crate::contract::StorageCommitFailure;
use crate::domain::{node_journal::Op, protocol::Command, session::Session};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn selection_side_session(&mut self, c: &Command) -> Result<Value> {
        let parent = c.session_id.clone().context("Session required")?;
        let first = c
            .payload
            .get("firstInput")
            .filter(|v| !v.is_null())
            .cloned();
        if let Some(input) = &first {
            ensure!(
                input["text"].as_str().is_some_and(|t| !t.trim().is_empty()),
                "Selection side chat input requires text"
            );
        }
        // 副屏读取父会话已落库的活动对话：先提交父会话尚未写入的记录。
        self.persist(&parent, None).await?;
        let s = &self.sessions[&parent];
        let selection = first
            .as_ref()
            .and_then(|i| i.get("modelSelection").filter(|m| m.is_object()).cloned())
            .or_else(|| s.node_selection());
        let active_turn = s
            .running()
            .then(|| s.node.turn.as_ref().map(|t| t.runtime.clone()))
            .flatten();
        let request = json!({"kind": "selection_side_chat", "parent": parent,
            "command": c.command_id, "revision": c.base_revision.unwrap_or(0),
            "selection": selection, "activeTurn": active_turn,
            "execution": {"mode": s.mode.as_str(), "planEnabled": s.plan_enabled}});
        let (now, revision) = (self.clock.now(), s.revision);
        let child = self.new_session_id();
        // 子会话与父会话的命令事实由同一个 fork 包事务写入；载体只承载这批写入。
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
        let mut ack = c.ack("accepted", revision, None);
        ack["result"] = json!({"type": "createSelectionSideSession", "sessionId": child});
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
        self.tools.inherit_session(&parent, &child).await?;
        let followup = self.sessions[&parent].followup_mode.clone();
        self.sessions.get_mut(&child).unwrap().followup_mode = followup;
        let Some(input) = first else {
            return Ok(ack);
        };
        // 首条输入只进入子会话（Node：同一命令 id 的账本行落在 child）。
        let mut command = c.clone();
        command.kind = "sendText".into();
        command.session_id = Some(child.clone());
        command.base_revision = None;
        command.base_log_epoch = None;
        command.payload = json!({"text": input["text"], "requestedDelivery": "startNow"});
        let admitted = self.send_input(command).await?;
        let result = &admitted["result"];
        let mut input = json!({"delivery": result["delivery"], "inputId": c.command_id});
        if let Some(message) = result.get("messageId").filter(|m| !m.is_null()) {
            input["messageId"] = message.clone();
        }
        ack["result"]["input"] = input;
        self.acks.insert(c.key(), ack.clone());
        Ok(ack)
    }
}
