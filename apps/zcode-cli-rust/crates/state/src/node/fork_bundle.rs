//! The records of Node's fork commit bundle (`buildForkedSessionInput`,
//! `buildAtomicForkNotice`, `cloneVerifierEntryForAtomicFork`, the child's
//! model selection and execution state entries, and the parent's command fact).
use super::entries::Entry;
use super::fork_clone::{Identities, local};
use super::sessions::{self, Create, SessionRow};
use anyhow::Result;
use serde_json::{Value, json};
use std::borrow::Cow;
use zcode_cli_domain::node_history::{Record, reminders};
use zcode_cli_domain::{node_ids, node_journal::records};

const AGENT: &str = "zcode-agent";
const EXECUTION_MODES: [&str; 4] = ["build", "edit", "yolo", "auto"];

/// Node `resolveExecutionState` of the last copied assistant, else the
/// parent runtime's current state.
pub fn execution(messages: &[Cow<Record>], runtime: &Value) -> Value {
    let Some(info) = messages
        .iter()
        .rev()
        .map(|m| &m.info)
        .find(|i| i["role"] == "assistant")
    else {
        return json!({"mode": runtime["mode"], "planEnabled": runtime["planEnabled"]});
    };
    let mode = info["mode"]
        .as_str()
        .filter(|m| EXECUTION_MODES.contains(m));
    let plan = match info.get("planEnabled").filter(|p| !p.is_null()) {
        Some(plan) => plan.clone(),
        None => (mode.is_none() && info["mode"] == "plan").into(),
    };
    json!({"mode": mode.unwrap_or("build"), "planEnabled": plan})
}

pub struct Bundle<'a> {
    pub parent: &'a SessionRow,
    pub ids: &'a Identities,
    pub selection: Option<&'a Value>,
    pub execution: &'a Value,
    pub command: &'a str,
    pub target: &'a Value,
    pub now: i64,
}

pub struct Notice {
    pub info: Value,
    pub parts: Vec<Value>,
}

fn entry(v: Value) -> Entry {
    super::apply::entry(&v)
}

impl Bundle<'_> {
    /// Node `buildForkedSessionInput`.
    pub fn create_child(&self, conn: &rusqlite::Connection) -> Result<()> {
        let p = self.parent;
        let slug = format!(
            "{}-fork-{}",
            node_ids::slugify(&p.slug),
            node_ids::base36(self.now as u64)
        );
        let create = Create {
            id: self.ids.child.clone(),
            project_id: p.project_id.clone(),
            workspace_id: p.workspace_id.clone(),
            parent_id: Some(p.id.clone()),
            trace_id: p.trace_id.clone(),
            task_type: Some("fork".into()),
            slug: slug.chars().take(120).collect(),
            directory: p.directory.clone(),
            path: p.path.clone(),
            title: format!("Fork of {}", p.title),
            title_source: Some("generated".into()),
            version: p.version.clone(),
            permission: p.permission.clone(),
            time_created: Some(self.now),
            time_updated: Some(self.now),
            ..Create::default()
        };
        sessions::create(conn, &create, self.now)?;
        Ok(())
    }

    fn fork_origin(&self, target_message: &str) -> Value {
        json!({"parentSessionId": self.parent.id, "targetMessageId": target_message})
    }

    /// Node `buildAtomicForkNotice`: the model-only notice and the visible
    /// separator, anchored as one stable segment.
    pub fn notice(&self, target_message: &str) -> Vec<Notice> {
        let (child, now) = (&self.ids.child, self.now as u64);
        let hidden = node_ids::message_id(now, &crate::id());
        let hidden_part = node_ids::part_id(now, &crate::id());
        let notice = node_ids::message_id(now, &crate::id());
        let notice_part = node_ids::part_id(now, &crate::id());
        let turn = node_ids::turn_id(&crate::id());
        let anchor_message = self
            .ids
            .messages
            .get(target_message)
            .cloned()
            .unwrap_or(hidden.clone());
        let anchor = json!({"turnId": turn, "productTurnId": hidden,
            "orderedMessageIds": [hidden, notice], "boundaryMessageId": notice});
        let origin = self.fork_origin(target_message);
        let mut user = json!({"id": hidden, "sessionID": child, "role": "user",
            "time": {"created": now}, "agent": AGENT});
        if let Some(selection) = self.selection {
            user["modelSelection"] = selection.clone();
        }
        user["synthetic"] = true.into();
        user["source"] = "fork".into();
        user["visibility"] = "model-only".into();
        user["semantics"] = json!({"origin": "system", "kind": "fork_notice", "uiVisibility": "hidden",
            "providerVisibility": "visible", "transcriptVisibility": "hidden"});
        user["anchor"] = anchor.clone();
        user["metadata"] = json!({"forkOrigin": origin});
        let body = reminders::sanitize(&[
            "This session was forked from a previous session message.".to_owned(),
            format!("parentSessionId: {}", self.parent.id),
            format!("targetMessageId: {target_message}"),
            "No workspace checkpoint was restored for this fork.".to_owned(),
            "Continue from this fork. Do not assume messages after the fork point happened in this session.".to_owned(),
        ].join("\n"));
        let text = json!({"id": hidden_part, "sessionID": child, "messageID": hidden, "type": "text",
            "text": body, "synthetic": true, "time": {"start": now, "end": now},
            "metadata": {"forkOrigin": origin, "runtimeMessage": {"source": "conversation_fork"},
                "source": "fork", "visibility": "model-only"}});
        let mut assistant = json!({"id": notice, "sessionID": child, "role": "assistant",
            "time": {"created": now, "completed": now}, "parentID": hidden});
        if let Some(selection) = self.selection {
            assistant["modelId"] = selection["modelId"].clone();
            assistant["providerId"] = selection["providerId"].clone();
            if let Some(level) = selection["options"]["reasoningLevel"]
                .as_str()
                .filter(|l| !l.is_empty())
            {
                assistant["reasoningLevel"] = level.into();
            }
        }
        assistant["mode"] = self.execution["mode"].clone();
        assistant["planEnabled"] = self.execution["planEnabled"].clone();
        assistant["agent"] = AGENT.into();
        assistant["path"] = json!({"cwd": self.parent.directory, "root": self.parent.directory});
        assistant["cost"] = 0.into();
        assistant["tokens"] = records::tokens(None);
        assistant["finish"] = "completed".into();
        assistant["semantics"] = json!({"origin": "system", "kind": "timeline_event", "uiVisibility": "visible",
            "providerVisibility": "hidden", "transcriptVisibility": "visible"});
        assistant["anchor"] = anchor;
        assistant["metadata"] = json!({"forkOrigin": origin});
        let separator = json!({"id": notice_part, "sessionID": child, "messageID": notice,
            "type": "timeline", "timelineType": "session_fork", "display": "separator",
            "status": "completed", "anchorMessageId": anchor_message, "anchorTurnId": turn,
            "sourceCommandId": self.command, "parentSessionId": self.parent.id,
            "targetMessageId": target_message, "restoredFileCount": 0,
            "time": {"start": now, "end": now}});
        vec![
            Notice {
                info: user,
                parts: vec![text],
            },
            Notice {
                info: assistant,
                parts: vec![separator],
            },
        ]
    }

    /// Node `cloneVerifierEntryForAtomicFork`.
    pub fn verifier(&self, source: &Entry) -> Result<Entry> {
        let ids = self.ids;
        let payload = &source.data["payload"];
        let mut next = payload.clone();
        next["targetId"] = local(
            &ids.targets,
            payload["targetId"].as_str().unwrap_or(""),
            "verifier target",
        )?
        .into();
        let verification = payload["verificationId"].as_str().unwrap_or("");
        next["verificationId"] = local(&ids.verifications, verification, "verification id")?.into();
        if let Some(anchor) = payload["anchorAssistantMessageId"].as_str() {
            next["anchorAssistantMessageId"] =
                local(&ids.messages, anchor, "verifier assistant anchor")?.into();
        }
        if let Some(turn) = payload["anchorTurnId"].as_str() {
            next["anchorTurnId"] = local(&ids.turns, turn, "verifier turn anchor")?.into();
        }
        let mut data = source.data.clone();
        data["eventId"] = crate::id().into();
        data["payload"] = next;
        data["forkOrigin"] = json!({"entryId": source.id, "eventId": source.data["eventId"],
            "verificationId": verification});
        Ok(Entry {
            id: local(&ids.verifier_entries, &source.id, "verifier entry")?,
            session_id: ids.child.clone(),
            data,
            ..source.clone()
        })
    }

    /// The child's model selection and execution state entries.
    pub fn entries(&self) -> Vec<Entry> {
        let now = self.now as u64;
        let child = &self.ids.child;
        let selection = self.selection.cloned().unwrap_or(Value::Null);
        let mode = self.execution["mode"].as_str().unwrap_or("build");
        let plan = self.execution["planEnabled"] == true;
        vec![
            entry(records::model_selection_entry(child, now, selection)),
            entry(records::execution_state_entry(child, now, mode, plan)),
        ]
    }

    /// Node `commitForkBundle`'s parent command fact.
    pub fn command_fact(&self, id: &str, revision: u64) -> Entry {
        let target_message = self.target["boundaryMessageId"].clone();
        let ack = json!({"commandId": self.command, "status": "accepted", "revisionAtDecision": revision,
            "result": {"type": "forkAssistant", "sessionId": self.ids.child}});
        let metadata = json!({"forkOrigin": {"parentSessionId": self.parent.id, "targetMessageId": target_message},
            "forkTarget": self.target});
        Entry {
            id: id.into(),
            session_id: self.parent.id.clone(),
            kind: "v4/command_fact".into(),
            time_created: self.now,
            time_updated: self.now,
            data: json!({"source": "child", "ack": ack, "metadata": metadata}),
            touch_session: true,
        }
    }
}
