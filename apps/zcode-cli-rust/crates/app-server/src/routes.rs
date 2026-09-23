//! Delivery routes of the App Server: subscription requests, flow control,
//! recovery scheduling and translation of runtime output into wire lines.
use super::server::{Pending, Server, not_owned, required, response};
use crate::contract::{ClientMsg, Method, RuntimeError, RuntimeEvent, ServerMsg};
use crate::domain::protocol::RequestId;
use anyhow::Result;
use serde_json::{Value, json};

/// Unwritten output above which conversation deltas are dropped and replaced by snapshots.
const HIGH_WATERMARK: usize = 64 * 1024 * 1024;
/// Backlog below which dropped subscriptions are recovered.
const LOW_WATERMARK: usize = 16 * 1024 * 1024;

impl Server {
    pub(super) fn subscribe(
        &mut self,
        id: Option<RequestId>,
        p: &Value,
    ) -> Result<Option<Value>, RuntimeError> {
        let topic = required(p, "topic")?.to_owned();
        let connection = required(p, "connectionId")?.to_owned();
        if !matches!(
            p["clientMode"].as_str(),
            Some("desktop-continuous" | "web-remote-replayable")
        ) {
            return Err(RuntimeError::InvalidParams("clientMode: invalid".into()));
        }
        self.forward(
            Method::TopicOpen,
            json!({"topic":topic}),
            Pending::Subscribe {
                id,
                topic,
                connection,
            },
        );
        Ok(None)
    }

    pub(super) fn resync(
        &mut self,
        id: Option<RequestId>,
        p: &Value,
    ) -> Result<Option<Value>, RuntimeError> {
        let subscription = required(p, "subscriptionId")?.to_owned();
        let topic = required(p, "topic")?;
        let connection = required(p, "connectionId")?;
        let owned = self
            .delivery
            .get(&subscription)
            .is_some_and(|s| s.topic == topic && s.connection == connection);
        if !owned {
            return Err(not_owned());
        }
        self.delivery.resume(&subscription);
        self.forward(
            Method::TopicSnapshot,
            json!({"topic":topic}),
            Pending::Resync { id, subscription },
        );
        Ok(None)
    }

    pub(super) fn unsubscribe(&mut self, p: &Value) -> Result<Value, RuntimeError> {
        let subscription = required(p, "subscriptionId")?;
        if let Some(sub) = self.delivery.get(subscription) {
            if p["connectionId"] != sub.connection.as_str() {
                return Err(not_owned());
            }
            let topic = self.delivery.unsubscribe(subscription).unwrap().topic;
            self.to_runtime
                .push_back(ClientMsg::TopicReleased { topic });
        }
        Ok(json!({}))
    }

    pub(super) fn flow(&mut self, p: &Value) -> Result<Value, RuntimeError> {
        let connection = required(p, "connectionId")?.to_owned();
        match required(p, "state")? {
            "closed" => {
                for topic in self.delivery.close_connection(&connection) {
                    self.to_runtime
                        .push_back(ClientMsg::TopicReleased { topic });
                }
                self.to_runtime
                    .push_back(ClientMsg::ConnectionClosed { connection });
            }
            "saturated" => self.delivery.set_paused(&connection, true),
            "drained" => {
                self.delivery.set_paused(&connection, false);
                // 暂停期间没有保留增量；用完整快照原子补齐，不能伪造连续水位。
                self.schedule_recovery(Some(&connection));
            }
            _ => return Err(RuntimeError::InvalidParams("state: invalid".into())),
        }
        Ok(json!({}))
    }

    fn schedule_recovery(&mut self, connection: Option<&str>) {
        if self.congested {
            return;
        }
        for (subscription, topic) in self.delivery.take_recoverable(connection) {
            self.forward(
                Method::TopicSnapshot,
                json!({"topic":topic}),
                Pending::Recover { subscription },
            );
        }
    }

    pub(super) async fn relieve(&mut self) -> Result<()> {
        if self.sink.backlog() < LOW_WATERMARK {
            self.congested = false;
            self.schedule_recovery(None);
        }
        Ok(())
    }

    pub(super) async fn on_runtime(&mut self, batch: Vec<ServerMsg>) -> Result<()> {
        let mut lines = vec![];
        for message in batch {
            match message {
                ServerMsg::Reply { token, result } => {
                    if let Some(pending) = self.calls.remove(&token) {
                        self.on_reply(pending, result, &mut lines)?;
                    }
                }
                ServerMsg::Event(event) => self.on_event(event, &mut lines)?,
                ServerMsg::HostRequest { id, method, params } => {
                    lines.push(json!({"id":id,"method":method,"params":params}).to_string());
                }
                ServerMsg::HostNotification { method, params } => {
                    lines.push(json!({"method":method,"params":params}).to_string());
                }
            }
        }
        self.sink.send(lines).await?;
        self.schedule_recovery(None);
        Ok(())
    }

    fn on_reply(
        &mut self,
        pending: Pending,
        result: Result<Value, RuntimeError>,
        lines: &mut Vec<String>,
    ) -> Result<()> {
        let seq = |v: &Value| v["seq"].as_u64().unwrap_or(0);
        match pending {
            Pending::Rpc(id) => lines.extend(id.map(|id| response(id, result))),
            Pending::Subscribe {
                id,
                topic,
                connection,
            } => match result {
                Ok(opened) => {
                    let (subscription, replaced) = self.delivery.subscribe(&topic, &connection);
                    if replaced {
                        // 同一连接重复订阅：旧订阅被替换，释放它持有的 pin。
                        self.to_runtime
                            .push_back(ClientMsg::TopicReleased { topic });
                    }
                    let ack = json!({"ack":{"subscriptionId":subscription,"mode":"snapshot","logEpoch":opened["epoch"]}});
                    lines.extend(id.map(|id| response(id, Ok(ack))));
                    lines.extend(self.delivery.snapshot(
                        &subscription,
                        "initial",
                        seq(&opened),
                        &opened["snapshot"],
                    )?);
                }
                Err(error) => lines.extend(id.map(|id| response(id, Err(error)))),
            },
            Pending::Resync { id, subscription } => match result {
                Ok(snapshot) if self.delivery.get(&subscription).is_some() => {
                    let ack = json!({"ack":{"subscriptionId":subscription,"mode":"snapshot","logEpoch":snapshot["epoch"]}});
                    lines.extend(id.map(|id| response(id, Ok(ack))));
                    lines.extend(self.delivery.snapshot(
                        &subscription,
                        "recovery",
                        seq(&snapshot),
                        &snapshot["snapshot"],
                    )?);
                }
                Ok(_) => lines.extend(id.map(|id| response(id, Err(not_owned())))),
                Err(error) => lines.extend(id.map(|id| response(id, Err(error)))),
            },
            Pending::Recover { subscription } => match result {
                Ok(snapshot) => lines.extend(self.delivery.snapshot(
                    &subscription,
                    "online",
                    seq(&snapshot),
                    &snapshot["snapshot"],
                )?),
                // 无法取得快照的订阅不能再保证连续性；结束它而不是反复重试。
                Err(_) => {
                    if let Some(sub) = self.delivery.unsubscribe(&subscription) {
                        self.to_runtime
                            .push_back(ClientMsg::TopicReleased { topic: sub.topic });
                    }
                }
            },
        }
        Ok(())
    }

    fn on_event(&mut self, event: RuntimeEvent, lines: &mut Vec<String>) -> Result<()> {
        match event {
            RuntimeEvent::ConversationDeltas {
                session,
                from,
                to,
                deltas,
            } => {
                if self.congested || self.sink.backlog() > HIGH_WATERMARK {
                    // 写队列积压：丢弃增量并要求快照恢复，绝不无界缓存或反压 actor。
                    self.congested = true;
                    self.delivery.invalidate(None);
                    return Ok(());
                }
                let payload = json!({"kind":"deltas","deltas":deltas}).to_string();
                lines.extend(self.delivery.deltas(
                    &format!("conversation/{session}"),
                    from,
                    to,
                    &payload,
                )?);
            }
            RuntimeEvent::ConversationReset {
                session,
                seq,
                snapshot,
            } => lines.extend(self.delivery.snapshot_topic(
                &format!("conversation/{session}"),
                "recovery",
                seq,
                &snapshot,
            )?),
            RuntimeEvent::TopicClosed { topic } => self.delivery.close_topic(&topic),
            RuntimeEvent::IndexChanged {
                workspace,
                from,
                to,
                delta,
            } => {
                let payload = json!({"kind":"deltas","deltas":[delta]}).to_string();
                lines.extend(self.delivery.deltas(
                    &format!("sessions-index/{workspace}"),
                    from,
                    to,
                    &payload,
                )?);
            }
            RuntimeEvent::ConfigChanged {
                workspace,
                seq,
                snapshot,
            } => lines.extend(self.delivery.snapshot_topic(
                &format!("workspace-config/{workspace}"),
                "online",
                seq,
                &snapshot,
            )?),
        }
        Ok(())
    }
}
