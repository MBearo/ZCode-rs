use super::Engine;
use crate::domain::{MAX_QUEUE, MAX_TOOL_BYTES, protocol::Command};
use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
impl Engine {
    pub(super) async fn compact_command(&mut self, c: &Command) -> Result<Value> {
        let id = c.session_id.as_deref().context("Session id required")?;
        let session = self.sessions.get(id).context("Session unavailable")?;
        if (session.mode != "yolo" || session.plan_enabled)
            || (self.model.is_none() && self.registry.is_none())
        {
            return Ok(c.ack(
                "rejected",
                session.revision,
                Some("guard.capabilityUnsupported"),
            ));
        }
        let instructions = c.payload["text"].as_str().unwrap_or("");
        if instructions.len() > MAX_TOOL_BYTES {
            bail!("Compaction instructions exceed limit");
        }
        let mut ack = c.ack("accepted", session.revision, None);
        let mut turn = None;
        if session.running() || !session.queue.is_empty() {
            if session.queue.len() >= MAX_QUEUE {
                return Ok(c.ack("rejected", session.revision, Some("guard.queueFull")));
            }
            let session = self.sessions.get_mut(id).unwrap();
            let mut item = super::commands::queue_item(c, session, self.clock.now());
            item["kind"] = "compact".into();
            item["text"] = instructions.into();
            ack["result"] =
                json!({"type":"inputAccepted","delivery":"queue","inputId":item["queueItemId"]});
            session.queue.push(item);
            session.revision += 1;
        } else {
            let (t, input) = self.admit_compact(id, c)?;
            turn = Some(t);
            ack["result"] = json!({"type":"inputAccepted","delivery":"startNow","inputId":input});
        }
        ack["revisionAtDecision"] = self.sessions[id].revision.into();
        let deltas = if turn.is_some() {
            vec![json!({"op":"row.appended","row":self.sessions[id].rows.last().unwrap()})]
        } else {
            vec![]
        };
        self.publish(id, deltas)?;
        self.persist(id, Some((c.key(), ack.clone()))).await?;
        self.acks.insert(c.key(), ack.clone());
        if let Some(turn) = turn {
            self.start_run(id, turn)?;
        }
        Ok(ack)
    }
    pub(super) fn admit_compact(&mut self, id: &str, c: &Command) -> Result<(String, String)> {
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        let now = self.clock.now();
        let turn = self.clock.id();
        s.compact_instructions = Some(c.payload["text"].as_str().unwrap_or("").into());
        s.run_id = Some(self.clock.id());
        s.phase = "running".into();
        s.last_error = None;
        s.updated_at = now;
        s.revision += 1;
        let mut header = s.row("turnHeader", &turn, &turn, now);
        header["origin"] = "userInput".into();
        header["executionKind"] = "controlOnly".into();
        header["state"] = "running".into();
        header["startedAt"] = now.into();
        header["sourceCommandId"] = c.command_id.clone().into();
        s.rows.push(header);
        Ok((turn.clone(), turn))
    }
}
