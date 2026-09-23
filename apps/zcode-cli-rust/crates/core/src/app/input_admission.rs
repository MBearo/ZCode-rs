use super::Engine;
use crate::domain::protocol::Command;
use anyhow::{Context, Result};
use serde_json::{Value, json};

impl Engine {
    pub(super) fn admit_input(
        &mut self,
        id: &str,
        c: &Command,
        shared: Option<String>,
    ) -> Result<(String, String)> {
        if c.kind == "sendText"
            && let Some(text) = c.payload["text"].as_str()
        {
            let text = text.trim();
            if text == "/compact" || text.starts_with("/compact ") {
                let mut compact = c.clone();
                compact.payload = json!({"text":text.strip_prefix("/compact").unwrap().trim()});
                return self.admit_compact(id, &compact);
            }
        }
        let selected = self.select(&c.payload, Some(self.session_selection(id)?))?;
        let mut content = self.input_content(id, &c.payload)?;
        if c.payload["_userSteer"] == true
            && let Some(text) = content.as_str()
        {
            content = crate::domain::prompt::user_steer(text).into();
        }
        self.apply_selection(id, selected)?;
        let s = self.sessions.get_mut(id).context("Session unavailable")?;
        if c.payload["planEnabled"] == false {
            s.plan_enabled = false;
        }
        let boundary = (
            s.rows.len(),
            s.messages.len(),
            crate::domain::history::State::capture(s),
        );
        let now = self.clock.now();
        let turn = self.clock.id();
        let input = self.clock.id();
        let text = c.payload["text"].as_str().context("Input text missing")?;
        if c.kind == "sendGoalCommand" {
            s.goal = Some(crate::domain::goal::Goal::new(
                self.clock.id(),
                text.trim().into(),
                now,
            ));
            content = s.goal.as_ref().unwrap().prompt("goalContinue", None).into();
        } else if let Some(goal) = s.goal.as_mut().filter(|g| g.active()) {
            goal.start(now);
        }
        super::shared_context::attach(
            s,
            &c.payload,
            shared,
            Some(&format!("queue_{}", c.command_id)),
            &input,
        )?;
        let retained_messages = s.messages.len();
        s.run_id = Some(self.clock.id());
        s.phase = "running".into();
        s.last_error = None;
        s.updated_at = now;
        s.revision += 1;
        if s.title.is_empty() {
            s.title = if text.trim().is_empty() {
                c.payload["attachments"][0]["fileName"]
                    .as_str()
                    .unwrap_or("Attachment")
                    .chars()
                    .take(80)
                    .collect()
            } else {
                text.chars().take(80).collect()
            };
            s.title_source = "generated".into();
        }
        let mut header = s.row("turnHeader", &turn, &turn, now);
        header["origin"] = if c.payload["_historyRerun"] == true {
            "editRerun"
        } else {
            "userInput"
        }
        .into();
        header["state"] = "running".into();
        header["startedAt"] = now.into();
        header["sourceCommandId"] = c.command_id.clone().into();
        s.rows.push(header);
        let mut row = s.row("userInput", &turn, &input, now);
        row["text"] = text.into();
        if c.kind == "sendGoalCommand" {
            row["text"] = c.payload["displayText"]
                .as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| format!("/goal {text}"))
                .into();
        }
        row["origin"] = "realUser".into();
        row["sourceCommandId"] = c.command_id.clone().into();
        row["clientId"] = c.client_id.clone().into();
        if let Some(refs) = c.payload.get("attachments") {
            row["attachments"] = refs.clone();
        }
        s.rows.push(row);
        if c.kind == "sendGoalCommand" {
            let mut marker = s.row("timelineMarker", &turn, &self.clock.id(), now);
            marker["marker"] = json!({"type":"goalSet","objective":text});
            s.rows.push(marker);
        }
        if !s.background.is_empty() {
            let statuses = s
                .background
                .values()
                .map(|t| json!({"task_id":t.id,"status":t.status,"outputFile":t.output_file}))
                .collect::<Vec<_>>();
            s.append_message(json!({"role":"user","content":format!("<task-notification>{}</task-notification>", serde_json::to_string(&statuses)?)}));
        }
        s.append_message(json!({"role":"user","content":content}));
        let mut payload = c.payload.clone();
        payload.as_object_mut().unwrap().remove("context_refs");
        s.history
            .inputs
            .push(crate::domain::history::InputBoundary {
                entity: input.clone(),
                turn: turn.clone(),
                row: boundary.0,
                user_row: boundary.0 + 1,
                message: retained_messages,
                state: boundary.2,
                kind: c.kind.clone(),
                payload,
            });
        Ok((turn, input))
    }
    pub(super) fn new_turn_rows(&self, id: &str) -> Vec<Value> {
        let rows = &self.sessions[id].rows;
        rows[self.sessions[id].current_rows_start()..]
            .iter()
            .map(|row| json!({"op":"row.appended","row":row}))
            .collect()
    }
}
