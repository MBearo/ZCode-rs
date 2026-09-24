use super::Engine;
use crate::{
    contract::{RuntimeError, StorageCommitFailure},
    domain::protocol::Command,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Engine {
    /// V4 `deleteSession`: closes the runtime session and records the command ACK.
    pub(super) async fn close_session(&mut self, c: &Command) -> Result<Value> {
        ensure!(
            c.payload.as_object().is_some_and(|p| p.is_empty()),
            "deleteSession requires an empty payload"
        );
        let id = c.session_id.as_deref().context("Session id required")?;
        ensure!(self.sessions.contains_key(id), "Session unavailable");
        let ack = self.shut_down(id, Some(c)).await?;
        Ok(ack.expect("command close returns an ACK"))
    }

    /// Legacy `session/close` (Node `closeSession`): same shutdown without an ACK,
    /// optionally conditional on the session's current persistence.
    pub(super) async fn close_runtime_session(&mut self, p: &Value) -> Result<Value> {
        let params = p
            .as_object()
            .filter(|o| {
                o.keys()
                    .all(|k| k == "sessionId" || k == "expectedPersistence")
            })
            .ok_or_else(|| RuntimeError::invalid_params("Unsupported session/close fields"))?;
        let id = params
            .get("sessionId")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| RuntimeError::invalid_params("sessionId: required"))?;
        let expected = match params.get("expectedPersistence") {
            None => None,
            Some(Value::String(v)) if v == "deferred" || v == "immediate" => Some(v.as_str()),
            Some(_) => return Err(RuntimeError::invalid_params("expectedPersistence: invalid")),
        };
        let Some(session) = self.sessions.get(id) else {
            return Err(RuntimeError::Coded {
                code: -32004,
                message: format!("Session is not active: {id}"),
            }
            .into());
        };
        // V4 草稿与 legacy deferred 首次输入前都是 deferred；legacy immediate 从创建起即 immediate。
        let immediate = session.runtime.persistence
            == Some(crate::domain::session_runtime::Persistence::Immediate);
        let current = if session.phase == crate::domain::execution::Phase::Draft && !immediate {
            "deferred"
        } else {
            "immediate"
        };
        // 连接切换与跨端首发可能并发；条件关闭在 owner 上原子判断，不依赖客户端旧快照。
        if expected.is_some_and(|expected| expected != current) {
            return Ok(json!({"closed": false}));
        }
        self.shut_down(id, None).await?;
        Ok(json!({"closed": true}))
    }

    /// Shared close path: cancels foreground and background work, releases waiters,
    /// uploads and subscriptions, reclaims history-less drafts and publishes the
    /// index removal. Persisted history stays in the store.
    async fn shut_down(&mut self, id: &str, command: Option<&Command>) -> Result<Option<Value>> {
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        s.auto_drain = false;
        if let Some(goal) = &mut s.goal {
            goal.finish_run(self.clock.now(), false, Some("paused"));
        }
        s.queued_now = None;
        self.close_topic(id);
        if let Some(active) = self.active.get(id) {
            active.cancel.cancel();
        }
        self.release_waiters(id);
        self.submissions.retain(|(session, _), _| session != id);
        self.cancel_children(id).await?;
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
        let ack = command.map(|c| (c.key(), c.ack("accepted", s.revision, None)));
        if s.rows.is_empty() && s.messages.is_empty() && s.shared_context.is_none() {
            self.store
                .discard_draft(&self.workspace, id, ack.clone())
                .await
                .context(StorageCommitFailure)?;
        } else {
            self.persist(id, ack.clone()).await?;
        }
        // 删除命令只关闭 runtime。历史保留在 Store，重开从新 epoch 冷恢复；失败不得发移除事实。
        self.uploads.0.retain(|key, _| key.1 != id);
        self.sessions.remove(id);
        self.hooks.sessions.remove(id);
        self.hooks.without.remove(id);
        self.session_access.remove(id);
        self.closed.insert(id.into());
        self.publish_index(id, None)?;
        Ok(ack.map(|(key, ack)| {
            self.acks.insert(key, ack.clone());
            ack
        }))
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
        super::file_changes::import(&mut session, self.tools.as_ref()).await;
        super::file_changes::hydrate(&mut session, self.tools.as_ref(), None).await?;
        self.store
            .commit_receipt(&self.workspace, Some(&mut session), None)
            .await
            .context(StorageCommitFailure)?;
        let summary = (session.listed && !session.archived).then(|| session.summary());
        self.sessions.insert(id.into(), session);
        self.touch_session(id);
        self.closed.remove(id);
        self.publish_index(id, summary)
    }

    pub(super) async fn conversation_query(
        &mut self,
        method: crate::contract::Method,
        p: &Value,
    ) -> Result<Value> {
        let id = p["sessionId"].as_str().context("Session id required")?;
        self.ensure_session(id).await?;
        let name = method.as_str();
        if name.contains("attachment") || name.starts_with("v4/attachment/") {
            self.attachment_query(name, p).await
        } else {
            self.query(method, p)
        }
    }
}
