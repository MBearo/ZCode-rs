//! Delivery routes of the App Server: subscription requests, flow control,
//! flush scheduling and translation of runtime output into wire lines.
//! Spec rust-m8-delivery.
use super::delivery::Output;
use super::server::{Pending, Server, not_owned, required, response};
use crate::contract::{ClientMsg, Method, RuntimeError, RuntimeEvent, ServerMsg};
use crate::domain::delivery::Profile;
use anyhow::Result;
use serde_json::{Value, json};
use std::time::Instant;

/// Unwritten output above which buffered deltas are dropped and replaced by snapshots.
const HIGH_WATERMARK: usize = 64 * 1024 * 1024;
/// Backlog below which dropped subscriptions are recovered.
const LOW_WATERMARK: usize = 16 * 1024 * 1024;

fn fault(error: &anyhow::Error) -> RuntimeError {
    RuntimeError::Fault {
        message: error.to_string(),
        code: None,
    }
}

impl Server {
    pub(super) fn subscribe(
        &mut self,
        id: Option<crate::domain::protocol::RequestId>,
        p: &Value,
    ) -> Result<Option<Value>, RuntimeError> {
        let topic = required(p, "topic")?.to_owned();
        let connection = required(p, "connectionId")?.to_owned();
        let Some(mode) = p["clientMode"]
            .as_str()
            .filter(|m| matches!(*m, "desktop-continuous" | "web-remote-replayable"))
        else {
            return Err(RuntimeError::InvalidParams("clientMode: invalid".into()));
        };
        self.forward(
            Method::TopicOpen,
            json!({"topic":topic,"base":p["base"]}),
            Pending::Subscribe {
                id,
                topic,
                connection,
                profile: Profile::from_client_mode(mode),
            },
        );
        Ok(None)
    }

    pub(super) fn resync(
        &mut self,
        id: Option<crate::domain::protocol::RequestId>,
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
        // 客户端 base 是唯一恢复起点；在途回复覆盖此后到达的全部增量。
        self.delivery.resync_started(&subscription);
        self.forward(
            Method::TopicSnapshot,
            json!({"topic":topic,"base":p["base"],"forceSnapshot":p["forceSnapshot"]}),
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
            "saturated" => self.delivery.pause(&connection),
            // 立即到期：事件循环随即刷新该连接的缓冲（需要时请求快照）。
            "drained" => self.delivery.drain(&connection, Instant::now()),
            _ => return Err(RuntimeError::InvalidParams("state: invalid".into())),
        }
        Ok(json!({}))
    }

    /// Queues the runtime requests and pin releases of a delivery step.
    fn route(&mut self, out: Output, lines: &mut Vec<String>) {
        lines.extend(out.lines);
        for (subscription, topic) in out.recover {
            self.forward(
                Method::TopicSnapshot,
                json!({"topic":topic}),
                Pending::Recover { subscription },
            );
        }
        for topic in out.released {
            self.to_runtime
                .push_back(ClientMsg::TopicReleased { topic });
        }
    }

    /// Frames of due subscriptions; above the writer backlog every buffer is
    /// dropped instead, never buffering without bound or blocking the actor.
    fn flush_into(&mut self, lines: &mut Vec<String>) {
        if !self.congested && self.sink.backlog() > HIGH_WATERMARK {
            self.congested = true;
            self.delivery.invalidate_all();
        }
        if self.congested {
            return;
        }
        let out = self.delivery.flush_due(Instant::now());
        self.route(out, lines);
    }

    /// A flush window ended.
    pub(super) async fn flush(&mut self) -> Result<()> {
        let mut lines = vec![];
        self.flush_into(&mut lines);
        self.sink.send(lines).await
    }

    /// Runtime output ended: write what subscribers still buffer.
    pub(super) async fn flush_final(&mut self) -> Result<()> {
        let lines = self.delivery.flush_all(Instant::now());
        self.sink.send(lines).await
    }

    pub(super) async fn relieve(&mut self) -> Result<()> {
        if self.sink.backlog() < LOW_WATERMARK {
            self.congested = false;
            self.delivery.wake(Instant::now());
        }
        Ok(())
    }

    pub(super) async fn on_runtime(&mut self, batch: Vec<ServerMsg>) -> Result<()> {
        let mut lines = vec![];
        for message in batch {
            match message {
                ServerMsg::Reply { token, result } => {
                    if let Some(pending) = self.calls.remove(&token) {
                        self.on_reply(pending, result, &mut lines);
                    }
                }
                ServerMsg::Event(event) => self.on_event(event, &mut lines),
                ServerMsg::HostRequest { id, method, params } => {
                    lines.push(json!({"id":id,"method":method,"params":params}).to_string());
                }
                ServerMsg::HostNotification { method, params } => {
                    lines.push(json!({"method":method,"params":params}).to_string());
                }
            }
        }
        // 窗口为 0 的订阅（sessions-index）在本批之后立即刷新。
        self.flush_into(&mut lines);
        self.sink.send(lines).await
    }

    fn on_reply(
        &mut self,
        pending: Pending,
        result: Result<Value, RuntimeError>,
        lines: &mut Vec<String>,
    ) {
        match pending {
            Pending::Rpc(id) => lines.extend(id.map(|id| response(id, result))),
            Pending::Subscribe {
                id,
                topic,
                connection,
                profile,
            } => {
                let state = match result {
                    Ok(state) => state,
                    Err(error) => return lines.extend(id.map(|id| response(id, Err(error)))),
                };
                match self
                    .delivery
                    .subscribe((&topic, &connection), profile, &state)
                {
                    Ok(subscribed) => {
                        if subscribed.replaced {
                            // 同一连接重复订阅：旧订阅被替换，释放它持有的 pin。
                            self.to_runtime.push_back(ClientMsg::TopicReleased {
                                topic: topic.clone(),
                            });
                        }
                        let ack = json!({"ack":{"subscriptionId":subscribed.id,"mode":subscribed.mode,"logEpoch":state["epoch"]}});
                        lines.extend(id.map(|id| response(id, Ok(ack))));
                        lines.extend(subscribed.lines);
                    }
                    Err(error) => {
                        // 首帧无法编码：不登记新订阅，释放 topicOpen 取得的 pin。
                        self.to_runtime
                            .push_back(ClientMsg::TopicReleased { topic });
                        lines.extend(id.map(|id| response(id, Err(fault(&error)))));
                    }
                }
            }
            Pending::Resync { id, subscription } => match result {
                Ok(state) if self.delivery.get(&subscription).is_some() => {
                    match self.delivery.recovered(&subscription, "recovery", &state) {
                        Ok(frames) => {
                            let ack = json!({"ack":{"subscriptionId":subscription,"mode":state["mode"],"logEpoch":state["epoch"]}});
                            lines.extend(id.map(|id| response(id, Ok(ack))));
                            lines.extend(frames);
                        }
                        Err(error) => {
                            lines.extend(id.map(|id| response(id, Err(fault(&error)))));
                        }
                    }
                }
                Ok(_) => lines.extend(id.map(|id| response(id, Err(not_owned())))),
                Err(error) => {
                    self.delivery.recovery_failed(&subscription, Instant::now());
                    lines.extend(id.map(|id| response(id, Err(error))));
                }
            },
            Pending::Recover { subscription } => {
                let frames = result.map_err(|e| anyhow::anyhow!(e.to_json().to_string()));
                match frames.and_then(|s| self.delivery.recovered(&subscription, "online", &s)) {
                    Ok(frames) => lines.extend(frames),
                    // 无法取得或编码快照的订阅不能再保证连续性；结束它而不是反复重试。
                    Err(_) => {
                        if let Some(sub) = self.delivery.unsubscribe(&subscription) {
                            self.to_runtime
                                .push_back(ClientMsg::TopicReleased { topic: sub.topic });
                        }
                    }
                }
            }
        }
    }

    fn on_event(&mut self, event: RuntimeEvent, lines: &mut Vec<String>) {
        let now = Instant::now();
        match event {
            // 写队列积压期间所有订阅都等待快照恢复，增量无需缓冲。
            RuntimeEvent::ConversationDeltas { .. } | RuntimeEvent::IndexChanged { .. }
                if self.congested => {}
            RuntimeEvent::ConversationDeltas {
                session,
                from,
                to,
                deltas,
                ttft,
            } => {
                let topic = format!("conversation/{session}");
                self.delivery.deltas(&topic, (from, to), &deltas, now);
                self.delivery.ttft(&topic, &ttft);
            }
            RuntimeEvent::IndexChanged {
                workspace,
                from,
                to,
                deltas,
            } => self.delivery.deltas(
                &format!("sessions-index/{workspace}"),
                (from, to),
                &deltas,
                now,
            ),
            RuntimeEvent::ConversationReset {
                session,
                seq,
                snapshot,
            } => {
                let out = self.delivery.snapshot_topic(
                    &format!("conversation/{session}"),
                    "recovery",
                    seq,
                    &snapshot,
                );
                self.route(out, lines);
            }
            RuntimeEvent::TopicClosed { topic } => self.delivery.close_topic(&topic),
            RuntimeEvent::ConfigChanged {
                workspace,
                seq,
                snapshot,
            } => {
                let out = self.delivery.snapshot_topic(
                    &format!("workspace-config/{workspace}"),
                    "online",
                    seq,
                    &snapshot,
                );
                self.route(out, lines);
            }
        }
    }
}
