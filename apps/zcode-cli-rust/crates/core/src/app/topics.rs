//! Actor-owned topic facts: snapshots, sequence numbers and delivery pins.
//!
//! Subscriptions, framing, flow control and fan-out belong to the frontend
//! (App Server). The actor only guarantees that a snapshot and its seq are
//! produced atomically and that every later change is emitted with a
//! contiguous `(from, to]` range while the topic is open.
use super::Engine;
use crate::contract::{RuntimeError, RuntimeEvent, ServerMsg};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

impl Engine {
    /// Delivery opened `topic`: load the session if needed, pin it and return a snapshot.
    pub(super) async fn open_topic(&mut self, p: &Value) -> Result<Value> {
        let topic = topic_param(p)?;
        if let Some(id) = topic.strip_prefix("conversation/") {
            self.ensure_session(id).await?;
        }
        // 先取快照再计数：快照失败（如 workspace 不匹配）不能留下 pin。
        let snapshot = self.topic_snapshot_value(p)?;
        *self.interest.entry(topic.to_owned()).or_default() += 1;
        Ok(snapshot)
    }

    /// One delivery subscription on `topic` ended.
    pub(super) fn release_topic(&mut self, topic: &str) {
        if let Some(count) = self.interest.get_mut(topic) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.interest.remove(topic);
            }
        }
    }

    /// `(epoch, seq, snapshot)` for a topic, as one consistent value.
    pub(super) fn topic_snapshot_value(&self, p: &Value) -> Result<Value> {
        let (epoch, seq, snapshot) = self.topic_snapshot(topic_param(p)?)?;
        Ok(json!({"epoch":epoch,"seq":seq,"snapshot":snapshot}))
    }

    fn topic_snapshot(&self, topic: &str) -> Result<(String, u64, Value)> {
        if let Some(id) = topic.strip_prefix("conversation/") {
            let session = self.sessions.get(id).context("Session unavailable")?;
            return Ok((session.epoch.clone(), session.seq, session.snapshot()));
        }
        if let Some(workspace) = topic.strip_prefix("sessions-index/") {
            if workspace != self.workspace {
                bail!("Workspace identity mismatch");
            }
            return Ok((self.epoch.clone(), self.index_seq, self.index_snapshot()));
        }
        if let Some(workspace) = topic.strip_prefix("workspace-config/") {
            if workspace != self.workspace {
                bail!("Workspace identity mismatch");
            }
            return Ok((self.epoch.clone(), self.config_seq, self.config_snapshot()));
        }
        bail!("Unsupported topic")
    }

    fn index_snapshot(&self) -> Value {
        json!({"protocolVersion":1,"workspaceId":self.workspace,"logEpoch":self.epoch,"sessions":self.index.values().collect::<Vec<_>>()})
    }

    fn config_snapshot(&self) -> Value {
        json!({"protocolVersion":1,"workspaceId":self.workspace,"logEpoch":self.epoch,"config":self.workspace_config()})
    }

    fn watched(&self, topic: &str) -> bool {
        self.interest.contains_key(topic)
    }

    pub(super) fn publish(&mut self, id: &str, mut deltas: Vec<Value>) -> Result<()> {
        let session = self.sessions.get_mut(id).context("Session unavailable")?;
        let text_only = deltas.iter().all(|d| d["op"] == "row.delta");
        if !text_only {
            deltas.extend(session.history_actions());
        }
        deltas.push(json!({"op":"state.updated","patch":session.patch()}));
        let from = session.seq;
        session.seq += deltas.len() as u64;
        let to = session.seq;
        // 纯文本增量不改变列表事实；lastActivityAt 在下一个语义边界随摘要一并发布，
        // 避免每个流式分片都推送一帧 sessions-index。
        let summary =
            (!text_only).then(|| (session.listed && !session.archived, session.summary()));
        if self.watched(&format!("conversation/{id}")) {
            self.outbox
                .push(ServerMsg::Event(RuntimeEvent::ConversationDeltas {
                    session: id.into(),
                    from,
                    to,
                    deltas,
                }));
        }
        match summary {
            Some((listed, summary)) => self.publish_index(id, listed.then_some(summary)),
            None => Ok(()),
        }
    }

    pub(super) fn publish_index(&mut self, id: &str, summary: Option<Value>) -> Result<()> {
        // 与 Node summariesEqual 一致：摘要未变化不推进 seq，也不产生帧。
        // 删除保持幂等广播：从未入索引的草稿关闭时，客户端仍需收到移除事实。
        if summary
            .as_ref()
            .is_some_and(|summary| self.index.get(id) == Some(summary))
        {
            return Ok(());
        }
        let delta = match summary {
            Some(summary) => {
                self.index.insert(id.into(), summary.clone());
                json!({"op":"session.upserted","session":summary})
            }
            None => {
                self.index.remove(id);
                json!({"op":"session.removed","sessionId":id})
            }
        };
        let from = self.index_seq;
        self.index_seq += 1;
        if self.watched(&format!("sessions-index/{}", self.workspace)) {
            self.outbox
                .push(ServerMsg::Event(RuntimeEvent::IndexChanged {
                    workspace: self.workspace.clone(),
                    from,
                    to: self.index_seq,
                    delta,
                }));
        }
        Ok(())
    }

    /// Configuration changed; `config_seq` has already advanced.
    pub(super) fn publish_config(&mut self) {
        if self.watched(&format!("workspace-config/{}", self.workspace)) {
            let snapshot = self.config_snapshot();
            self.outbox
                .push(ServerMsg::Event(RuntimeEvent::ConfigChanged {
                    workspace: self.workspace.clone(),
                    seq: self.config_seq,
                    snapshot,
                }));
        }
    }

    /// History was rewritten under a new epoch: subscribers replace their state.
    pub(super) fn reset_topic(&mut self, id: &str) -> Result<()> {
        if self.watched(&format!("conversation/{id}")) {
            let session = self.sessions.get(id).context("Session unavailable")?;
            self.outbox
                .push(ServerMsg::Event(RuntimeEvent::ConversationReset {
                    session: id.into(),
                    seq: session.seq,
                    snapshot: session.snapshot(),
                }));
        }
        let summary = self.sessions[id].summary();
        self.publish_index(id, Some(summary))
    }

    /// The conversation topic ends (session deleted); drop its pin and subscriptions.
    pub(super) fn close_topic(&mut self, id: &str) {
        let topic = format!("conversation/{id}");
        if self.interest.remove(&topic).is_some() {
            self.outbox
                .push(ServerMsg::Event(RuntimeEvent::TopicClosed { topic }));
        }
    }

    pub(super) fn pinned_topics(&self) -> impl Iterator<Item = &str> {
        self.interest
            .keys()
            .filter_map(|topic| topic.strip_prefix("conversation/"))
    }
}

fn topic_param(p: &Value) -> Result<&str> {
    p["topic"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RuntimeError::invalid_params("topic: required"))
}
