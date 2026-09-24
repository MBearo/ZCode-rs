//! Actor-owned topic facts: snapshots, sequence numbers, retained logs and
//! delivery pins.
//!
//! Subscriptions, framing, flow control and fan-out belong to the frontend
//! (App Server). The actor only guarantees that a snapshot and its seq are
//! produced atomically and that every later change is emitted with a
//! contiguous `(from, to]` range while the topic is open.
use super::Engine;
use crate::contract::{RuntimeError, RuntimeEvent, ServerMsg};
use crate::domain::topic_log::sized;
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};

impl Engine {
    /// Delivery opened `topic`: load the session if needed, pin it and return
    /// its resume range or snapshot.
    pub(super) async fn open_topic(&mut self, p: &Value) -> Result<Value> {
        let topic = topic_param(p)?;
        if let Some(id) = topic.strip_prefix("conversation/") {
            self.ensure_session(id).await?;
        }
        // 先取快照再计数：快照失败（如 workspace 不匹配）不能留下 pin。
        let state = self.topic_snapshot_value(p)?;
        *self.interest.entry(topic.to_owned()).or_default() += 1;
        Ok(state)
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

    /// `{epoch, seq, mode, deltas | snapshot}` of a topic as one consistent
    /// value: `resume` with the retained deltas after `base`, or `snapshot`
    /// (spec rust-m8-delivery §3).
    pub(super) fn topic_snapshot_value(&mut self, p: &Value) -> Result<Value> {
        let topic = topic_param(p)?;
        let base = (p["forceSnapshot"] != true)
            .then(|| {
                p["base"]["logEpoch"]
                    .as_str()
                    .zip(p["base"]["seq"].as_u64())
            })
            .flatten();
        let resumable = |epoch: &str| base.filter(|(e, _)| *e == epoch).map(|(_, seq)| seq);
        if let Some(id) = topic.strip_prefix("conversation/") {
            // 差量 patch 的基线必须等于快照中的 state：先发布尚未发布的 state 变化。
            self.publish(id, vec![])?;
            let session = self.sessions.get(id).context("Session unavailable")?;
            let replay = resumable(&session.epoch).and_then(|seq| {
                let deltas = session.topic.log.replay(&session.epoch, seq, session.seq)?;
                Some((seq, deltas))
            });
            return Ok(reply(&session.epoch, session.seq, replay, || {
                session.snapshot()
            }));
        }
        if let Some(workspace) = topic.strip_prefix("sessions-index/") {
            if workspace != self.workspace {
                bail!("Workspace identity mismatch");
            }
            let replay = resumable(&self.epoch).and_then(|seq| {
                let deltas = self.index_log.replay(&self.epoch, seq, self.index_seq)?;
                Some((seq, deltas))
            });
            return Ok(reply(&self.epoch, self.index_seq, replay, || {
                self.index_snapshot()
            }));
        }
        if let Some(workspace) = topic.strip_prefix("workspace-config/") {
            if workspace != self.workspace {
                bail!("Workspace identity mismatch");
            }
            // 整体替换态：只有水位对齐时才续传，否则快照（两者等价）。
            let replay = resumable(&self.epoch)
                .filter(|seq| *seq == self.config_seq)
                .map(|seq| (seq, vec![]));
            return Ok(reply(&self.epoch, self.config_seq, replay, || {
                self.config_snapshot()
            }));
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
        // 客户端按在场键整体替换 patch：只发变化的顶层键，流式文本块因此只剩 row.delta。
        let patch = session.patch();
        if let Some(patch) = session.topic.changed(&session.epoch, patch) {
            deltas.push(json!({"op":"state.updated","patch":patch}));
        }
        // 纯文本增量不改变列表事实；lastActivityAt 在下一个语义边界随摘要一并发布，
        // 避免每个流式分片都推送一帧 sessions-index。
        let summary =
            (!text_only).then(|| (session.listed && !session.archived, session.summary()));
        if !deltas.is_empty() {
            let from = session.seq;
            session.seq += deltas.len() as u64;
            let to = session.seq;
            let deltas = sized(deltas);
            // 日志始终记账，与订阅无关：手机重连时即使无人订阅过也能续传。
            session
                .topic
                .log
                .record(&session.epoch, from, to, deltas.clone());
            if self.watched(&format!("conversation/{id}")) {
                self.outbox
                    .push(ServerMsg::Event(RuntimeEvent::ConversationDeltas {
                        session: id.into(),
                        from,
                        to,
                        deltas,
                    }));
            }
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
        let deltas = sized(vec![delta]);
        self.index_log
            .record(&self.epoch, from, self.index_seq, deltas.clone());
        if self.watched(&format!("sessions-index/{}", self.workspace)) {
            self.outbox
                .push(ServerMsg::Event(RuntimeEvent::IndexChanged {
                    workspace: self.workspace.clone(),
                    from,
                    to: self.index_seq,
                    deltas,
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
        let session = self.sessions.get_mut(id).context("Session unavailable")?;
        // 所有订阅者都以这份快照为准：差量基线直接对齐当前 state，不另发 patch。
        let patch = session.patch();
        session.topic.changed(&session.epoch, patch);
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

/// `resume` carries the retained deltas after `from` (the client's base).
fn reply(
    epoch: &str,
    seq: u64,
    replay: Option<(u64, Vec<Value>)>,
    snapshot: impl FnOnce() -> Value,
) -> Value {
    match replay {
        Some((from, deltas)) => {
            json!({"epoch":epoch,"seq":seq,"mode":"resume","from":from,"deltas":deltas})
        }
        None => json!({"epoch":epoch,"seq":seq,"mode":"snapshot","snapshot":snapshot()}),
    }
}

fn topic_param(p: &Value) -> Result<&str> {
    p["topic"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| RuntimeError::invalid_params("topic: required"))
}
