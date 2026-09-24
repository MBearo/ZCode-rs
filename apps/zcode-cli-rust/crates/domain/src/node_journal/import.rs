//! Claude Code history imports (Node `persistImportedSessionHistory`,
//! `claudeCode`): the session row, earlier imported messages removed, and one
//! user or assistant message with its text part per imported message.
//! Spec rust-m11-node-storage §5.6.
use super::Op;
use crate::session::Session;
use serde_json::{Value, json};

const SOURCE: &str = "claudeCode";

impl Session {
    /// Node's import records of `history`; `model` is the bound model
    /// (`providerId`, `modelId`) when one was given.
    pub fn node_claude_import(
        &mut self,
        now: u64,
        history: &Value,
        model: Option<(&str, &str)>,
        version: &str,
    ) {
        let created = crate::claude_import::created_at(history, now);
        let messages = history["messages"].as_array().cloned().unwrap_or_default();
        let updated = history["updatedAt"]
            .as_u64()
            .or_else(|| messages.last().and_then(|m| m["timestamp"].as_u64()))
            .unwrap_or(created);
        let directory = self.node_directory().to_owned();
        let mut create =
            json!({"id": self.id, "projectID": crate::node_ids::app_project_id(&directory)});
        if self.workspace != directory {
            create["workspaceID"] = self.workspace.clone().into();
        }
        if let Some(trace) = &self.trace_id {
            create["traceID"] = trace.clone().into();
        }
        create["slug"] = crate::node_ids::import_slug(&self.id).into();
        create["directory"] = directory.clone().into();
        create["path"] = directory.clone().into();
        create["title"] = self.title.clone().into();
        create["titleSource"] = "custom".into();
        create["version"] = version.into();
        create["permission"] = json!({"mode": self.mode.as_str()});
        create["time"] = json!({"created": created, "updated": created.max(updated)});
        self.node.push(now, Op::CreateSession(create));
        self.node.created = true;
        self.node.push(now, Op::RemoveImported(SOURCE.into()));
        let mut previous = created.wrapping_sub(1);
        let mut user: Option<String> = None;
        for (index, message) in messages.iter().enumerate() {
            let raw = message["timestamp"]
                .as_u64()
                .unwrap_or(created + index as u64);
            // 与 Node 一致：按数组顺序把时间收敛为严格递增，保证读取顺序不反转。
            let ts = if raw > previous || previous == u64::MAX {
                raw
            } else {
                previous + 1
            };
            previous = ts;
            let id = format!("msg_{}_import_{index}", self.id);
            let info = if message["role"] == "user" {
                user = Some(id.clone());
                let mut info = json!({"id": id, "sessionID": self.id, "role": "user",
                    "time": {"created": ts}, "agent": "zcode-agent"});
                if let Some((provider, model)) = model {
                    info["modelSelection"] = json!({"providerId": provider, "modelId": model});
                }
                info["metadata"] = json!({"migrationSource": SOURCE});
                info
            } else {
                let parent = user
                    .clone()
                    .unwrap_or_else(|| format!("msg_{}_import_parent_{index}", self.id));
                let mut info = json!({"id": id, "sessionID": self.id, "role": "assistant",
                    "time": {"created": ts, "completed": ts}, "parentID": parent});
                if let Some((provider, model)) = model {
                    info["modelId"] = model.into();
                    info["providerId"] = provider.into();
                }
                info["mode"] = self.mode.as_str().into();
                info["agent"] = "zcode-agent".into();
                info["path"] = json!({"cwd": directory, "root": directory});
                info["cost"] = 0.into();
                info["tokens"] = super::records::tokens(None);
                info["finish"] = "stop".into();
                info
            };
            let part = json!({"id": format!("part_{}_import_{index}_text", self.id), "sessionID": self.id,
                "messageID": id, "type": "text", "text": message["content"],
                "time": {"start": ts, "end": ts}, "metadata": {"migrationSource": SOURCE}});
            self.node.push(now, Op::Message(info));
            self.node.push(now, Op::Part(part));
        }
    }
}
