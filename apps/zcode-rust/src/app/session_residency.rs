use super::Engine;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeSet;

impl Engine {
    pub(super) fn touch_session(&mut self, id: &str) {
        self.access_seq += 1;
        self.session_access.insert(id.into(), self.access_seq);
    }
    pub(super) async fn cached_ack(&mut self, key: &str) -> Result<Option<Value>> {
        if let Some(ack) = self.acks.get(key) {
            return Ok(Some(ack.clone()));
        }
        let ack = self.store.lookup_ack(&self.workspace, key).await?;
        if let Some(ack) = &ack {
            self.acks.insert(key.into(), ack.clone());
            self.durable_acks.insert(key.into());
        }
        Ok(ack)
    }
    pub(super) async fn query_acks(&mut self, p: &Value) -> Result<Value> {
        self.validate_workspace(p)?;
        let keys = p["commands"].as_array().context("Command keys required")?;
        ensure!(
            !keys.is_empty() && keys.len() <= 64,
            "Invalid command query size"
        );
        let mut results = vec![];
        for key in keys {
            let encoded = serde_json::to_string(&(
                key["sessionId"].as_str(),
                key["commandId"].as_str().context("Command ID required")?,
            ))?;
            results.push(json!({"key":key,"result":self.cached_ack(&encoded).await?.unwrap_or(json!("unknown"))}));
        }
        Ok(json!({"results":results}))
    }
    pub(super) async fn read_cold_session(&self, p: &Value) -> Result<Value> {
        self.validate_workspace(p)?;
        let id = p["sessionId"].as_str().context("Session id required")?;
        if self.sessions.contains_key(id) {
            return self.read_session(p);
        }
        ensure!(!self.closed.contains(id), "Session unavailable");
        let mut session = self
            .store
            .load_session(&self.workspace, id)
            .await?
            .context("Session unavailable")?;
        ensure!(
            session.workspace == self.workspace && session.context.offset <= session.messages.len(),
            "Invalid persisted session boundary"
        );
        // Host 索引观察只生成临时只读投影，不激活 runtime，不改历史或订阅 epoch。
        session.validate_history()?;
        session.recover(self.clock.id(), self.clock.now());
        self.read_session_snapshot(&session, p)
    }
    pub(super) async fn trim_resident(&mut self) -> Result<()> {
        let mut pinned: BTreeSet<String> = self
            .subscriptions
            .values()
            .filter_map(|s| s.topic.strip_prefix("conversation/").map(str::to_owned))
            .collect();
        pinned.extend(self.uploads.0.keys().map(|key| key.1.clone()));
        pinned.extend(
            self.sessions
                .iter()
                .filter(|(id, s)| {
                    self.active.contains_key(*id)
                        || s.phase == "draft"
                        || !s.queue.is_empty()
                        || !s.pending.is_empty()
                        || !s.mailbox.is_empty()
                        || s.children.values().any(|t| t.running() || !t.notified)
                        || s.background.values().any(|t| t.status == "running")
                })
                .map(|(id, _)| id.clone()),
        );
        let mut idle = self
            .sessions
            .iter_mut()
            .filter(|(id, _)| !pinned.contains(*id))
            .map(|(id, session)| {
                (
                    self.session_access.get(id).copied().unwrap_or(0),
                    id.clone(),
                    session.estimated_resident_bytes(),
                )
            })
            .collect::<Vec<_>>();
        idle.sort();
        let mut count = idle.len();
        let mut bytes = idle
            .iter()
            .fold(0usize, |sum, (_, _, size)| sum.saturating_add(*size));
        for (_, id, size) in idle {
            if count <= 8 && bytes <= 16 * 1024 * 1024 {
                break;
            }
            count -= 1;
            bytes = bytes.saturating_sub(size);
            self.tools.evict_session(&id).await?;
            self.sessions.remove(&id);
            self.session_access.remove(&id);
        }
        if self.acks.len() > 1024 {
            let queued = self
                .sessions
                .values()
                .flat_map(|s| {
                    s.queue.iter().filter_map(|q| {
                        q["sourceCommandId"]
                            .as_str()
                            .map(|id| serde_json::to_string(&(Some(&s.id), id)).unwrap())
                    })
                })
                .collect::<BTreeSet<_>>();
            self.acks
                .retain(|key, _| !self.durable_acks.contains(key) || queued.contains(key));
            self.durable_acks.retain(|key| self.acks.contains_key(key));
        }
        self.child_updates
            .retain(|_, watch| watch.receiver_count() > 0 || watch.borrow().running());
        Ok(())
    }
}
