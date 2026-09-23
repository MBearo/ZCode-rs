use super::Engine;
use crate::{contract::StorageCommitFailure, domain::protocol::Command};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn close_session(&mut self, c: &Command) -> Result<Value> {
        ensure!(
            c.payload.as_object().is_some_and(|p| p.is_empty()),
            "deleteSession requires an empty payload"
        );
        let id = c.session_id.as_deref().context("Session id required")?;
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        s.auto_drain = false;
        if let Some(goal) = &mut s.goal {
            goal.pause(self.clock.now());
        }
        s.queued_now = None;
        self.subscriptions
            .retain(|_, sub| sub.topic != format!("conversation/{id}"));
        if let Some(active) = self.active.get(id) {
            active.cancel.cancel();
        }
        self.cancel_auth(id);
        self.cancel_children(id).await?;
        self.permissions.retain(|_, (owner, _)| owner != id);
        self.questions.retain(|_, q| q.session != id);
        self.tools.cancel_session(id, None).await?;
        // TS close 会释放执行资源；必须收齐真正的终态，不能提前 ACK 后让 Shell 继续写文件。
        // 同时消费其他会话事件，避免有界事件通道阻塞取消后的终态投递。
        while self.active.contains_key(id)
            || self.sessions[id].children.values().any(|t| t.running())
            || self.sessions[id]
                .background
                .values()
                .any(|task| task.status == "running")
        {
            let event = self.event_rx.recv().await.context("Run channel closed")?;
            self.apply_event(event).await?;
        }
        self.tools.close_session(id).await?;
        let s = self.sessions.get_mut(id).unwrap();
        if let Some(context) = &mut s.shared_context {
            context.release(None);
        }
        for item in std::mem::take(&mut s.queue) {
            let key = serde_json::to_string(&(Some(id), item["sourceCommandId"].as_str()))?;
            if let Some(ack) = self.acks.get_mut(&key) {
                ack["status"] = "failed".into();
                ack["reasonCode"] = "fault.input.discardedOnClose".into();
                ack["result"] =
                    json!({"type":"inputDisposition","delivery":ack["result"]["delivery"]});
                s.pending_acks.insert(key, ack.clone());
            }
        }
        s.revision += 1;
        let ack = c.ack("accepted", s.revision, None);
        if s.rows.is_empty() && s.messages.is_empty() && s.shared_context.is_none() {
            self.store
                .discard_draft(&self.workspace, id, (c.key(), ack.clone()))
                .await
                .context(StorageCommitFailure)?;
        } else {
            self.persist(id, Some((c.key(), ack.clone()))).await?;
        }
        // 删除命令只关闭 runtime。历史保留在 Store，重开从新 epoch 冷恢复；失败不得发移除事实。
        self.uploads.0.retain(|key, _| key.1 != id);
        self.sessions.remove(id);
        self.session_access.remove(id);
        self.closed.insert(id.into());
        self.publish_index(id, None)?;
        self.acks.insert(c.key(), ack.clone());
        Ok(ack)
    }

    pub(super) async fn ensure_session(&mut self, id: &str) -> Result<()> {
        if self.sessions.contains_key(id) {
            self.touch_session(id);
            return Ok(());
        }
        let mut session = self
            .store
            .load_session(&self.workspace, id)
            .await?
            .context("Session unavailable")?;
        ensure!(
            session.workspace == self.workspace && session.context.offset <= session.messages.len(),
            "Invalid persisted session boundary"
        );
        session.validate_history()?;
        session.recover(self.clock.id(), self.clock.now());
        super::file_changes::hydrate(&mut session, self.tools.as_ref(), None).await?;
        self.store
            .commit(&self.workspace, Some(&mut session), None)
            .await
            .context(StorageCommitFailure)?;
        let summary = (session.listed && !session.archived).then(|| session.summary());
        self.sessions.insert(id.into(), session);
        self.touch_session(id);
        self.closed.remove(id);
        self.publish_index(id, summary)
    }

    pub(super) async fn conversation_query(&mut self, method: &str, p: &Value) -> Result<Value> {
        let id = p["sessionId"].as_str().context("Session id required")?;
        self.ensure_session(id).await?;
        if method.contains("attachment") || method.starts_with("v4/attachment/") {
            self.attachment_query(method, p).await
        } else {
            self.query(method, p)
        }
    }
}
