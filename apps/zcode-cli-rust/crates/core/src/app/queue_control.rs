use super::Engine;
use crate::domain::protocol::Command;
use anyhow::{Context, Result};
use serde_json::{Value, json};
impl Engine {
    pub(super) async fn send_queued_now(&mut self, c: &Command) -> Result<Value> {
        let id = c.session_id.as_deref().context("Session required")?;
        let session = self.sessions.get_mut(id).context("Session unavailable")?;
        if (session.mode != "yolo" || session.plan_enabled)
            || (self.model.is_none() && self.registry.is_none())
        {
            return Ok(c.ack("rejected", session.revision, Some("guard.modelUnavailable")));
        }
        if session.queued_now.is_some() {
            return Ok(c.ack(
                "rejected",
                session.revision,
                Some("guard.queuePromotionBusy"),
            ));
        }
        let item_id = c.payload["queueItemId"]
            .as_str()
            .context("Queue id required")?;
        let Some(item) = session
            .queue
            .iter_mut()
            .find(|q| q["queueItemId"] == item_id)
        else {
            return Ok(c.ack("noop", session.revision, Some("queue.itemMissing")));
        };
        item["dispatch"] = json!({"state":"reserved","reservationId":c.command_id});
        session.queued_now = Some(item_id.into());
        session.revision += 1;
        let ack = c.ack("accepted", session.revision, None);
        self.publish(id, vec![])?;
        self.persist(id, Some((c.key(), ack.clone()))).await?;
        self.acks.insert(c.key(), ack.clone());
        if let Some(active) = self.active.get(id) {
            active.cancel.cancel();
        }
        self.promote(id).await?;
        Ok(ack)
    }
    pub(super) fn apply_held_queue(
        &mut self,
        id: &str,
        payload: &Value,
    ) -> Result<Option<&'static str>> {
        let session = self.sessions.get(id).context("Session unavailable")?;
        if session.running() || session.auto_drain || session.queue.is_empty() {
            return Ok(None);
        }
        let Some(disposition) = payload["heldQueueDisposition"].as_str() else {
            return Ok(Some("heldQueueDispositionRequired"));
        };
        if let Some(expected) = payload.get("expectedHeldQueueItemIds") {
            let expected = expected
                .as_array()
                .context("Expected queue ids must be an array")?;
            let ids = expected
                .iter()
                .filter_map(Value::as_str)
                .collect::<std::collections::BTreeSet<_>>();
            if ids.len() != expected.len()
                || ids.len() != session.queue.len()
                || session
                    .queue
                    .iter()
                    .any(|q| !ids.contains(q["queueItemId"].as_str().unwrap()))
            {
                return Ok(Some("guard.heldQueueConfirmationStale"));
            }
        }
        if disposition == "clearQueueAndSend" {
            let items = std::mem::take(&mut self.sessions.get_mut(id).unwrap().queue);
            for item in items {
                self.discard_queue_ack(id, &item)?;
            }
        }
        Ok(None)
    }
    pub(super) fn discard_queue_ack(&mut self, id: &str, item: &Value) -> Result<()> {
        super::shared_context::release(self.sessions.get_mut(id).unwrap(), item);
        let key = serde_json::to_string(&(Some(id), item["sourceCommandId"].as_str().unwrap()))?;
        if let Some(ack) = self.acks.get_mut(&key) {
            ack["status"] = "failed".into();
            ack["reasonCode"] = "guard.queueDeleted".into();
            ack["result"] = json!({"type":"inputDisposition","delivery":ack["result"]["delivery"]});
            self.sessions
                .get_mut(id)
                .unwrap()
                .pending_acks
                .insert(key, ack.clone());
        }
        Ok(())
    }
    pub(super) async fn promote(&mut self, id: &str) -> Result<()> {
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        if s.running() || (!s.auto_drain && s.queued_now.is_none()) || s.queue.is_empty() {
            return Ok(());
        }
        let pos = s
            .queued_now
            .as_ref()
            .and_then(|id| s.queue.iter().position(|q| q["queueItemId"] == *id))
            .unwrap_or(0);
        let item = s.queue[pos].clone();
        let mut c = Command {
            command_id: item["sourceCommandId"].as_str().unwrap().into(),
            client_id: item["clientId"].as_str().unwrap().into(),
            session_id: Some(id.into()),
            base_revision: None,
            base_log_epoch: None,
            kind: item["kind"].as_str().unwrap_or("sendText").into(),
            payload: json!({"text":item["text"],"attachments":item["attachments"],"modelSelection":item["modelSelection"],"mode":item["mode"],"planEnabled":item["planEnabled"],"_userSteer":item["delivery"]["admitted"]=="startNow"}),
            issued_at: 0.0,
        };
        if let Some(refs) = item.get("sharedContextRefs") {
            c.payload["context_refs"] = refs.clone();
        }
        if self
            .select(&c.payload, Some(self.session_selection(id)?))
            .is_err()
        {
            // 配置可能在排队后失效；保留输入与冻结选型，不能移除输入后使整个 actor 退出。
            let s = self.sessions.get_mut(id).unwrap();
            s.auto_drain = false;
            s.queued_now = None;
            s.queue[pos]["dispatch"] = json!({"state":"queued"});
            s.last_error = Some(
                json!({"code":"model_not_found","message":"Queued model is unavailable; restore its configuration or remove this queued input.","recoverable":true,"source":"runtime","at":self.clock.now()}),
            );
            s.revision += 1;
            self.publish(id, vec![])?;
            self.persist(id, None).await?;
            return Ok(());
        }
        let shared = self
            .shared_input(id, &c.payload, item["queueItemId"].as_str())
            .await?;
        let s = self.sessions.get_mut(id).unwrap();
        s.queue.remove(pos);
        s.queued_now = None;
        let (turn, deltas) = if item["kind"] == "compact" {
            let (turn, _) = self.admit_compact(id, &c)?;
            (
                turn,
                vec![json!({"op":"row.appended","row":self.sessions[id].rows.last().unwrap()})],
            )
        } else {
            let (turn, _) = self.admit_input(id, &c, shared)?;
            (turn, self.new_turn_rows(id))
        };
        self.publish(id, deltas)?;
        self.persist(id, None).await?;
        self.start_run(id, turn)
    }
}
