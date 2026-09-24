//! Shared context imports (Node `persistImportedSessionHistory` with
//! `commitSharedContextImportBundle`, and `transitionSharedContextImport`).
//! Spec rust-m11-node-storage §5.6.
use super::Op;
use crate::session::Session;
use serde_json::{Value, json};

/// The import's facts beyond the session's own.
pub struct Import<'a> {
    pub markdown: &'a str,
    /// The provenance as imported (`{shareId, contextId, status, ...}`).
    pub provenance: Value,
    pub version: &'a str,
}

impl Session {
    /// Node `commitSharedContextImportBundle`: the session row, the model-only
    /// context message and the `v4/shared_context_import` provenance entry.
    pub fn node_shared_import(&mut self, now: u64, i: Import) {
        let created = self.created_at;
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
        create["path"] = directory.into();
        create["title"] = self.title.clone().into();
        create["titleSource"] = "custom".into();
        create["version"] = i.version.into();
        create["permission"] = json!({"mode": self.mode.as_str()});
        create["time"] = json!({"created": created, "updated": created});
        self.node.push(now, Op::CreateSession(create));
        self.node.created = true;
        let context = i.provenance["contextId"].clone();
        let status = i.provenance["status"].clone();
        let message = format!("msg_{}_shared_context", self.id);
        let mut info = json!({"id": message, "sessionID": self.id, "role": "user",
            "time": {"created": created}, "agent": "zcode-agent"});
        if let Some(selection) = self.node_selection() {
            info["modelSelection"] = selection;
        }
        info["synthetic"] = true.into();
        info["source"] = "shared_context".into();
        info["visibility"] = "model-only".into();
        info["semantics"] = json!({"origin": "import", "kind": "shared_context",
            "source": "conversation_share", "uiVisibility": "hidden", "providerVisibility": "visible",
            "transcriptVisibility": "visible"});
        info["metadata"] = json!({"shareId": i.provenance["shareId"], "contextId": context,
            "sharedContextStatus": status});
        let part = json!({"id": format!("part_{}_shared_context_text", self.id), "sessionID": self.id,
            "messageID": message, "type": "text", "text": i.markdown,
            "time": {"start": created, "end": created}, "metadata": {"sharedContext": true}});
        let share = i.provenance["shareId"].as_str().unwrap_or("");
        let entry = json!({"id": format!("v4_shared_context_import:{}:{share}", self.id),
            "sessionID": self.id, "type": "v4/shared_context_import",
            "time": {"created": created, "updated": created}, "data": i.provenance});
        self.node.push(now, Op::Message(info));
        self.node.push(now, Op::Part(part));
        self.node.push(now, Op::Entry(entry));
    }

    /// Node `transitionSharedContextImport` from one of `from` to `to`.
    pub fn node_shared_transition(
        &mut self,
        now: u64,
        context: &str,
        (from, to): (&[&str], &str),
        source: Option<&str>,
    ) {
        if !self.node.created {
            return;
        }
        self.node.push(
            now,
            Op::SharedTransition {
                context: context.into(),
                from: from.iter().map(|s| s.to_string()).collect(),
                to: to.into(),
                source: source.map(str::to_owned),
            },
        );
    }
}
