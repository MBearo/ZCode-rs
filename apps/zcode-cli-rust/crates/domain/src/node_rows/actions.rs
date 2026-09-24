//! Command row actions of the replayed snapshot (Node
//! `ProductProjection.materializeCommandRowActions`): an action is shown only
//! when the command layer can resolve its durable target.
use super::projection::{Delta, Projection, row_id};
use serde_json::{Map, Value};
use std::collections::HashMap;

fn toggle(actions: &mut Map<String, Value>, key: &str, on: bool) {
    if on {
        actions.insert(key.into(), true.into());
    } else {
        actions.shift_remove(key);
    }
}

impl Projection {
    fn has_message(&self, row: &Value) -> bool {
        self.message_by_row.contains_key(&row_id(row))
    }

    /// The latest real-user row whose canonical edit target resolves.
    fn latest_editable(&self, compact_active: bool) -> Option<u64> {
        if compact_active {
            return None;
        }
        let row = self
            .rows
            .iter()
            .rev()
            .find(|row| row["kind"] == "userInput" && row["origin"] == "realUser")?;
        let entity = self.entity_by_row.get(&row_id(row))?;
        (self.has_message(row) && self.edit_targets.contains_key(entity)).then(|| row_id(row))
    }

    /// The latest assistant text, when it completed a finished turn whose real
    /// user input is still an edit target.
    fn latest_retryable(&self, blocking: bool) -> Option<u64> {
        let latest = self
            .rows
            .iter()
            .rev()
            .find(|row| row["kind"] == "assistantText")?;
        if blocking || latest["state"] != "complete" || !self.has_message(latest) {
            return None;
        }
        let turn = latest["turnId"].as_str().unwrap_or("");
        let header = self.headers.get(turn).and_then(|id| self.find_row(*id))?;
        if header["kind"] != "turnHeader" || header["state"] == "running" {
            return None;
        }
        let user = self.rows.iter().find(|row| {
            row["turnId"] == turn && row["kind"] == "userInput" && row["origin"] == "realUser"
        })?;
        let entity = self.entity_by_row.get(&row_id(user))?;
        self.edit_targets
            .contains_key(entity)
            .then(|| row_id(latest))
    }

    pub fn materialize_actions(&mut self) -> Vec<Delta> {
        let works = self.state["control"]["activeWorks"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let compact_active = works.iter().any(|w| w["kind"] == "compact");
        let blocking = !works.is_empty();
        let no_pending = self.state["pendingInteractions"]
            .as_array()
            .is_none_or(Vec::is_empty);
        let mut latest_assistant_by_turn: HashMap<&str, u64> = HashMap::new();
        for row in self
            .rows
            .iter()
            .filter(|row| row["kind"] == "assistantText")
        {
            let entry = latest_assistant_by_turn
                .entry(row["turnId"].as_str().unwrap_or(""))
                .or_insert(0);
            *entry = (*entry).max(row_id(row));
        }
        let editable = self.latest_editable(compact_active);
        let retryable = self.latest_retryable(blocking);
        let mut deltas = Vec::new();
        for row in &self.rows {
            let kind = row["kind"].as_str().unwrap_or("");
            if !matches!(kind, "turnHeader" | "userInput" | "assistantText") {
                continue;
            }
            let mut actions: Map<String, Value> =
                row["actions"].as_object().cloned().unwrap_or_default();
            match kind {
                "turnHeader" => toggle(
                    &mut actions,
                    "canRewindFiles",
                    !blocking
                        && no_pending
                        && row["state"] != "running"
                        && row["fileChanges"]["state"] == "active",
                ),
                "userInput" => {
                    let on = Some(row_id(row)) == editable;
                    toggle(&mut actions, "canEdit", on);
                    if on {
                        actions.insert("editDisposition".into(), "rewind".into());
                    } else {
                        actions.shift_remove("editDisposition");
                    }
                }
                _ => {
                    toggle(&mut actions, "canRetry", Some(row_id(row)) == retryable);
                    let turn = row["turnId"].as_str().unwrap_or("");
                    let header = self.headers.get(turn).and_then(|id| self.find_row(*id));
                    let can_fork = !compact_active
                        && row["state"] == "complete"
                        && header.is_some_and(|h| {
                            h["kind"] == "turnHeader" && h["state"] == "completedSuccess"
                        })
                        && latest_assistant_by_turn.get(turn) == Some(&row_id(row))
                        && self.has_message(row);
                    toggle(&mut actions, "canFork", can_fork);
                }
            }
            let next = (!actions.is_empty()).then_some(Value::Object(actions));
            if next.as_ref() == row.get("actions") {
                continue;
            }
            let mut row = row.clone();
            match next {
                Some(actions) => row["actions"] = actions,
                None => {
                    row.as_object_mut()
                        .expect("rows are objects")
                        .shift_remove("actions");
                }
            }
            deltas.push(Delta::Upsert(row));
        }
        self.current_editable = editable.and_then(|id| self.entity_by_row.get(&id)).cloned();
        deltas
    }
}
