use super::Engine;
use anyhow::{Context, Result, bail};
use base64::Engine as _;
use serde_json::{Value, json};

pub(super) struct Subscription {
    pub id: String,
    pub topic: String,
    pub connection: String,
    pub ordinal: u64,
    pub paused: bool,
    pub needs_resync: bool,
}
impl Engine {
    pub(super) async fn subscribe(&mut self, p: &Value) -> Result<Value> {
        let topic = string(p, "topic")?.to_owned();
        let connection = string(p, "connectionId")?.to_owned();
        if !matches!(
            p["clientMode"].as_str(),
            Some("desktop-continuous" | "web-remote-replayable")
        ) {
            bail!("Invalid clientMode");
        }
        if let Some(id) = topic.strip_prefix("conversation/") {
            self.ensure_session(id).await?;
        }
        self.topic_snapshot(&topic)?;
        self.subscriptions
            .retain(|_, s| s.topic != topic || s.connection != connection);
        let id = self.clock.id();
        self.subscriptions.insert(
            id.clone(),
            Subscription {
                id: id.clone(),
                topic: topic.clone(),
                connection,
                ordinal: 0,
                paused: false,
                needs_resync: false,
            },
        );
        let (epoch, _, _) = self.topic_snapshot(&topic)?;
        self.snapshot_frame(&id, "initial")?;
        Ok(json!({"ack":{"subscriptionId":id,"mode":"snapshot","logEpoch":epoch}}))
    }
    pub(super) fn resync(&mut self, p: &Value) -> Result<Value> {
        let id = string(p, "subscriptionId")?;
        let topic = string(p, "topic")?;
        let connection = string(p, "connectionId")?;
        let sub = self
            .subscriptions
            .get_mut(id)
            .context("Subscription unavailable")?;
        if sub.topic != topic || sub.connection != connection {
            bail!("Subscription owner mismatch");
        }
        sub.needs_resync = false;
        sub.paused = false;
        let (epoch, _, _) = self.topic_snapshot(topic)?;
        self.snapshot_frame(id, "recovery")?;
        Ok(json!({"ack":{"subscriptionId":id,"mode":"snapshot","logEpoch":epoch}}))
    }
    pub(super) fn unsubscribe(&mut self, p: &Value) -> Result<Value> {
        let id = string(p, "subscriptionId")?;
        if let Some(sub) = self.subscriptions.get(id)
            && p["connectionId"] != sub.connection
        {
            bail!("Subscription owner mismatch");
        }
        self.subscriptions.remove(id);
        Ok(json!({}))
    }
    pub(super) fn connection_flow(&mut self, p: &Value) -> Result<Value> {
        let connection = string(p, "connectionId")?;
        let state = string(p, "state")?;
        if state == "closed" {
            self.uploads.clear_connection(connection);
            self.subscriptions.retain(|_, s| s.connection != connection);
        } else {
            if !matches!(state, "drained" | "saturated") {
                bail!("Invalid flow state");
            }
            for sub in self
                .subscriptions
                .values_mut()
                .filter(|s| s.connection == connection)
            {
                sub.paused = state == "saturated";
                if sub.paused {
                    sub.needs_resync = true;
                }
            }
            if state == "drained" {
                let ids = self
                    .subscriptions
                    .values()
                    .filter(|s| s.connection == connection && s.needs_resync)
                    .map(|s| s.id.clone())
                    .collect::<Vec<_>>();
                // 没有保留暂停区间的增量时，用完整 snapshot 原子补齐；不能伪造连续水位。
                for id in ids {
                    self.snapshot_frame(&id, "online")?;
                    self.subscriptions.get_mut(&id).unwrap().needs_resync = false;
                }
            }
        }
        Ok(json!({}))
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
            return Ok((
                self.epoch.clone(),
                self.index_seq,
                json!({"protocolVersion":1,"workspaceId":workspace,"logEpoch":self.epoch,"sessions":self.index.values().collect::<Vec<_>>()}),
            ));
        }
        if let Some(workspace) = topic.strip_prefix("workspace-config/") {
            if workspace != self.workspace {
                bail!("Workspace identity mismatch");
            }
            return Ok((
                self.epoch.clone(),
                self.config_seq,
                json!({"protocolVersion":1,"workspaceId":workspace,"logEpoch":self.epoch,"config":self.workspace_config()}),
            ));
        }
        bail!("Unsupported topic")
    }
    pub(super) fn snapshot_frame(&mut self, id: &str, delivery: &str) -> Result<()> {
        let topic = self
            .subscriptions
            .get(id)
            .context("Subscription unavailable")?
            .topic
            .clone();
        let (_, seq, snapshot) = self.topic_snapshot(&topic)?;
        self.push_frame(
            id,
            delivery,
            0,
            seq,
            json!({"kind":"snapshot","snapshot":snapshot}),
        )
    }
    fn push_frame(
        &mut self,
        id: &str,
        delivery: &str,
        from: u64,
        to: u64,
        payload: Value,
    ) -> Result<()> {
        let sub = self
            .subscriptions
            .get_mut(id)
            .context("Subscription unavailable")?;
        sub.ordinal += 1;
        let frame = json!({"topic":sub.topic,"subscriptionId":sub.id,"fromSeq":from,"toSeq":to,"sentAt":self.clock.now(),"payload":payload});
        let wire = json!({"wireVersion":3,"kind":"complete","deliveryKind":delivery,"logicalFrameId":self.clock.id(),"logicalFrameOrdinal":sub.ordinal,
            "topic":sub.topic,"subscriptionId":sub.id,"frame":frame});
        self.outbox.extend(encode(wire)?);
        Ok(())
    }
    pub(super) fn publish(&mut self, id: &str, mut deltas: Vec<Value>) -> Result<()> {
        let session = self.sessions.get_mut(id).context("Session unavailable")?;
        if !deltas.iter().all(|d| d["op"] == "row.delta") {
            deltas.extend(session.history_actions());
        }
        deltas.push(json!({"op":"state.updated","patch":session.patch()}));
        let from = session.seq;
        session.seq += deltas.len() as u64;
        let to = session.seq;
        let summary = session.summary();
        let listed = session.listed && !session.archived;
        let topic = format!("conversation/{id}");
        let ids = self
            .subscriptions
            .values()
            .filter(|s| s.topic == topic && !s.paused && !s.needs_resync)
            .map(|s| s.id.clone())
            .collect::<Vec<_>>();
        for sub in ids {
            self.push_frame(
                &sub,
                "online",
                from,
                to,
                json!({"kind":"deltas","deltas":deltas}),
            )?;
        }
        self.publish_index(id, listed.then_some(summary))
    }
    pub(super) fn publish_index(&mut self, id: &str, summary: Option<Value>) -> Result<()> {
        if let Some(summary) = &summary {
            self.index.insert(id.into(), summary.clone());
        } else {
            self.index.remove(id);
        }
        let from = self.index_seq;
        self.index_seq += 1;
        let topic = format!("sessions-index/{}", self.workspace);
        let ids = self
            .subscriptions
            .values()
            .filter(|s| s.topic == topic && !s.paused && !s.needs_resync)
            .map(|s| s.id.clone())
            .collect::<Vec<_>>();
        for sub in ids {
            self.push_frame(
                &sub,
                "online",
                from,
                self.index_seq,
                json!({"kind":"deltas","deltas":[if let Some(summary)=&summary{json!({"op":"session.upserted","session":summary})}else{json!({"op":"session.removed","sessionId":id})}]}),
            )?;
        }
        Ok(())
    }
}
fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .context("Missing subscription field")
}
fn encode(wire: Value) -> Result<Vec<Value>> {
    let notification = |params| json!({"method":"v4/conversation/frame","params":params});
    if serde_json::to_vec(&wire)?.len() < 900 * 1024 {
        return Ok(vec![notification(wire)]);
    }
    let data = serde_json::to_vec(&wire["frame"])?;
    if data.len() > 16 * 1024 * 1024 {
        bail!("Projection exceeds logical frame limit");
    }
    let checksum = format!("{:08x}", crc32fast::hash(&data));
    let chunk_size = 512 * 1024;
    let count = data.len().div_ceil(chunk_size);
    Ok(data
        .chunks(chunk_size)
        .enumerate()
        .map(|(i, chunk)| {
            let mut part = wire.clone();
            part.as_object_mut().unwrap().remove("frame");
            part["kind"] = "fragment".into();
            part["fragmentIndex"] = i.into();
            part["fragmentCount"] = count.into();
            part["logicalBytes"] = data.len().into();
            part["checksum"] = json!({"algorithm":"crc32","value":checksum});
            part["dataBase64"] = base64::engine::general_purpose::STANDARD
                .encode(chunk)
                .into();
            notification(part)
        })
        .collect())
}
