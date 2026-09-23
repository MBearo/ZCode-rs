use super::{Engine, commands::queue_item};
use crate::domain::{MAX_QUEUE, protocol::Command, session::Session};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::sync::oneshot;

impl Engine {
    pub(super) async fn send_input(&mut self, mut c: Command) -> Result<Value> {
        let id = c.session_id.clone().context("Session required")?;
        let s = &self.sessions[&id];
        if s.mode != "yolo" || (s.plan_enabled && c.payload["planEnabled"] != false) {
            return Ok(c.ack("rejected", s.revision, Some("guard.capabilityUnsupported")));
        }
        self.validate_input(&c.payload)?;
        if let Some(reference) = crate::domain::shared_context::reference(&c.payload)? {
            s.shared_context
                .as_ref()
                .context("fault.command.sharedContextNotAttachable")?
                .check(&reference, None)?;
            c.payload["context_refs"] =
                json!([{"kind":"shared_context_import","context_id":reference}]);
        }
        let shared = if s.running() {
            None
        } else {
            self.shared_input(&id, &c.payload, None).await?
        };
        let start_now = s.running() && c.payload["requestedDelivery"] == "startNow";
        if start_now && s.queued_now.is_some() {
            return Ok(c.ack("rejected", s.revision, Some("guard.queuePromotionBusy")));
        }
        if s.running() && s.queue.len() >= MAX_QUEUE {
            return Ok(c.ack("rejected", s.revision, Some("guard.queueFull")));
        }
        let selected = self.select(&c.payload, Some(self.session_selection(&id)?))?;
        let assets = self
            .prepare_attachments(&id, &mut c.payload, &selected)
            .await?;
        c.payload["modelSelection"] = json!({"providerId":selected.provider_id,"modelId":selected.model_id,"options":{"reasoningLevel":selected.reasoning_level}});
        if let Some(reason) = self.apply_held_queue(&id, &c.payload)? {
            return Ok(c.ack("rejected", self.sessions[&id].revision, Some(reason)));
        }
        let s = self.sessions.get_mut(&id).unwrap();
        s.attachments.extend(assets);
        let mut ack = c.ack("accepted", s.revision, None);
        let turn = if s.running() {
            if c.payload.get("requestedDelivery").is_none() {
                c.payload["requestedDelivery"] = s.followup_mode.clone().into();
            }
            let mut item = queue_item(&c, s, self.clock.now());
            super::shared_context::reserve(s, &c.payload, item["queueItemId"].as_str().unwrap())?;
            if start_now {
                item["delivery"]["admitted"] = "startNow".into();
                item["dispatch"] = json!({"state":"reserved","reservationId":c.command_id});
                s.queued_now = item["queueItemId"].as_str().map(str::to_owned);
            } else if c.payload["requestedDelivery"] == "guide" {
                if item["attachments"]
                    .as_array()
                    .is_some_and(|a| !a.is_empty())
                {
                    fallback(&mut item, "guide.attachmentsUnsupported");
                } else {
                    item["delivery"]["admitted"] = "guide".into();
                    item["steer"] = json!({"state":"steering"});
                }
            }
            ack["result"] = json!({"type":"inputAccepted","delivery":if start_now{"startNow"}else{"queue"},"inputId":item["queueItemId"]});
            s.queue.push(item);
            s.revision += 1;
            self.publish(&id, vec![])?;
            None
        } else {
            let (turn, input_id) = self.admit_input(&id, &c, shared)?;
            ack["result"] =
                json!({"type":"inputAccepted","delivery":"startNow","inputId":input_id});
            self.publish(&id, self.new_turn_rows(&id))?;
            Some(turn)
        };
        ack["revisionAtDecision"] = self.sessions[&id].revision.into();
        self.persist(&id, Some((c.key(), ack.clone()))).await?;
        self.acks.insert(c.key(), ack.clone());
        if start_now {
            // ACK 与预留先落盘，旧轮 Finished 才能提升，不能在取消时提前执行新输入。
            self.active[&id].cancel.cancel();
            self.cancel_auth(&id);
        }
        if let Some(turn) = turn {
            self.start_run(&id, turn)?;
        }
        Ok(ack)
    }

    pub(super) async fn drain_guide(
        &mut self,
        id: &str,
        turn: &str,
        committed: oneshot::Sender<Option<Vec<Value>>>,
    ) -> Result<()> {
        let session = &self.sessions[id];
        let pos = session.queue.iter().position(|item| {
            item["delivery"]["admitted"] == "guide" && item["dispatch"]["state"] == "queued"
        });
        let Some(pos) = pos.filter(|_| session.queued_now.is_none()) else {
            let _ = committed.send(None);
            return Ok(());
        };
        let item = session.queue[pos].clone();
        let mut input = json!({});
        if let Some(refs) = item.get("sharedContextRefs") {
            input["context_refs"] = refs.clone();
        }
        let shared = self
            .shared_input(id, &input, item["queueItemId"].as_str())
            .await?;
        let selection = match self.select(&item, Some(self.session_selection(id)?)) {
            Ok(selection) => selection,
            Err(_) => {
                // 排队后配置可能被删除；保留输入，交由已有 queue 配置失效处理，不丢失 guide。
                fallback(
                    &mut self.sessions.get_mut(id).unwrap().queue[pos],
                    "guide.noToolBoundary",
                );
                self.publish(id, vec![])?;
                self.persist(id, None).await?;
                let _ = committed.send(None);
                return Ok(());
            }
        };
        self.apply_selection(id, selection)?;
        let s = self.sessions.get_mut(id).unwrap();
        s.queue.remove(pos);
        s.revision += 1;
        s.updated_at = self.clock.now();
        let boundary = (
            s.rows.len(),
            s.messages.len(),
            crate::domain::history::State::capture(s),
        );
        let mut row = s.row("userInput", turn, &self.clock.id(), s.updated_at);
        let mut messages = vec![];
        if let Some(message) = super::shared_context::attach(
            s,
            &input,
            shared,
            item["queueItemId"].as_str(),
            row["entityId"].as_str().unwrap(),
        )? {
            messages.push(message);
        }
        let retained_messages = s.messages.len();
        row["text"] = item["text"].clone();
        row["origin"] = "realUser".into();
        row["guided"] = true.into();
        row["sourceCommandId"] = item["sourceCommandId"].clone();
        row["clientId"] = item["clientId"].clone();
        s.rows.push(row.clone());
        let message = json!({"role":"user","content":crate::domain::prompt::user_steer(item["text"].as_str().context("Guide text missing")?)});
        s.append_message(message.clone());
        messages.push(message);
        s.history.inputs.push(crate::domain::history::InputBoundary {entity:row["entityId"].as_str().unwrap().into(),turn:turn.into(),row:boundary.0,user_row:boundary.0,message:retained_messages,state:boundary.2,kind:"sendText".into(),payload:json!({"text":item["text"],"modelSelection":item["modelSelection"],"_userSteer":true})});
        self.publish(id, vec![json!({"op":"row.appended","row":row})])?;
        self.persist(id, None).await?;
        self.notify_selection(id)?;
        let _ = committed.send(Some(messages));
        Ok(())
    }
}

pub(super) fn fallback_guides(session: &mut Session, reason: &str) {
    for item in &mut session.queue {
        if item["delivery"]["admitted"] == "guide" {
            fallback(item, reason);
        }
    }
}
fn fallback(item: &mut Value, reason: &str) {
    item["delivery"]["admitted"] = "queue".into();
    item["delivery"]["fallbackReasonCode"] = reason.into();
    item["steer"] = json!({"state":"fellBack","reasonCode":reason});
}
