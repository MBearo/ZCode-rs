//! Node workspace checkpoints of file-mutating tools (`emitFileMutationCheckpoint`,
//! `persistWorkspaceCheckpointEntry`, `persistWorkspaceFileRewindEntry`):
//! the before-change artifact of one tool result and the entries that let a
//! resumed session preview and undo it. Spec rust-m11-node-storage §5.5.
use super::Op;
use crate::file_checkpoint::NodeCheckpoint;
use crate::session::Session;
use serde_json::{Map, Value, json};

/// Node `WORKSPACE_CHECKPOINT_CONTENT_TYPE`.
pub const CONTENT_TYPE: &str = "application/vnd.zcode.workspace-checkpoint+json";

/// Node `isRuntimeDiffHunk`.
fn hunk(value: &Value) -> bool {
    ["oldStart", "oldLines", "newStart", "newLines"]
        .iter()
        .all(|key| value[key].is_number())
        && value["lines"]
            .as_array()
            .is_some_and(|lines| lines.iter().all(Value::is_string))
}

/// Node `getFileMutationCheckpointCandidate` over a tool's structured output.
pub fn candidate(output: &Value) -> Option<Value> {
    let path = output["filePath"].as_str().filter(|p| !p.is_empty())?;
    let patch = output["structuredPatch"].as_array()?;
    if !patch.iter().all(hunk) {
        return None;
    }
    let original = &output["originalFile"];
    if !original.is_null() && !original.is_string() {
        return None;
    }
    let mut out = Map::new();
    if let Some(content) = output["content"].as_str() {
        out.insert("content".into(), content.into());
    }
    out.insert("filePath".into(), path.into());
    out.insert("originalFile".into(), original.clone());
    out.insert("structuredPatch".into(), Value::Array(patch.clone()));
    if let Some(kind) = output["type"].as_str() {
        out.insert("type".into(), kind.into());
    }
    Some(Value::Object(out))
}

/// Node `stringifyWorkspaceCheckpointArtifact` (`JSON.stringify(_, null, 2)`).
pub fn artifact(candidate: &Value, (call, tool): (&str, &str), now: u64) -> String {
    let mut file = Map::new();
    if let Some(content) = candidate["content"].as_str() {
        file.insert("afterContent".into(), content.into());
        file.insert(
            "afterContentLength".into(),
            content.encode_utf16().count().into(),
        );
    }
    file.insert("beforeContent".into(), candidate["originalFile"].clone());
    file.insert(
        "existedBefore".into(),
        (!candidate["originalFile"].is_null()).into(),
    );
    file.insert("path".into(), candidate["filePath"].clone());
    file.insert(
        "structuredPatch".into(),
        candidate["structuredPatch"].clone(),
    );
    let artifact = json!({"createdAt": crate::hooks::input::iso_timestamp(now),
        "files": [Value::Object(file)], "kind": "workspace_file_before_change",
        "toolCallId": call, "toolName": tool, "version": 1});
    serde_json::to_string_pretty(&artifact).unwrap_or_default()
}

impl Session {
    /// Node `CheckpointCreated` of the current tool step (`messageId` is the
    /// turn's user message, `toolMessageId` the step's assistant), stored as a
    /// `runtime/workspace_checkpoint` entry.
    pub fn node_checkpoint(
        &mut self,
        now: u64,
        (event, trace, checkpoint): (String, String, String),
        uri: &str,
    ) -> Option<NodeCheckpoint> {
        if !self.node.created {
            return None;
        }
        let turn = self.node.turn.as_ref()?;
        let recorded = NodeCheckpoint {
            checkpoint: format!("checkpoint_{checkpoint}"),
            message: turn.user.clone(),
            snapshot: uri.into(),
        };
        let mut payload = json!({"checkpointId": recorded.checkpoint,
            "messageId": turn.user, "targetMessageId": turn.user});
        if let Some(step) = &turn.step {
            payload["toolMessageId"] = step.assistant.clone().into();
        }
        payload["scope"] = "workspace".into();
        payload["snapshotRef"] = uri.into();
        payload["diffRef"] = uri.into();
        payload["fileCount"] = 1.into();
        let data = json!({"eventId": event, "payload": payload, "sequenceNumber": self.revision,
            "traceId": trace, "turnId": turn.runtime});
        let entry = json!({"id": format!("workspace-checkpoint:{event}"), "sessionID": self.id,
            "type": "runtime/workspace_checkpoint", "time": {"created": now, "updated": now},
            "data": data});
        self.node.push(now, Op::Entry(entry));
        Some(recorded)
    }

    /// Node `CheckpointCreated` of tool call `call`, remembered on the Rust
    /// file checkpoint of its tool row. Node's event carries the step's
    /// assistant (`toolMessageId`), so it precedes the tool part.
    pub fn node_tool_checkpoint(
        &mut self,
        now: u64,
        ids: (String, String, String),
        uri: &str,
        call: &str,
    ) {
        let recorded = self.node_checkpoint(now, ids, uri);
        let row = self
            .rows
            .iter()
            .find(|r| r["toolCallId"] == call)
            .and_then(|r| r["rowId"].as_u64());
        let change = self
            .file_checkpoints
            .iter_mut()
            .rev()
            .find(|c| Some(c.row) == row && c.node.is_none());
        if let Some(change) = change {
            change.node = recorded;
        }
    }

    /// Node file summary rewind (`RewindTriggered`, `reason: file_summary_rewind`)
    /// of `restored` checkpoints in order, stored as `runtime/workspace_file_rewind`.
    pub fn node_file_rewind(
        &mut self,
        now: u64,
        (rewind, event, trace): (String, String, String),
        restored: &[NodeCheckpoint],
    ) {
        let (Some(first), Some(last)) = (restored.first(), restored.last()) else {
            return;
        };
        if !self.node.created {
            return;
        }
        let payload = json!({"rewindId": format!("rewind_{rewind}"), "scope": "workspace",
            "strategy": "active_chain", "targetMessageId": first.message,
            "targetCheckpointId": last.checkpoint, "restoredSnapshotRef": last.snapshot,
            "reason": "file_summary_rewind"});
        let data = json!({"eventId": event, "payload": payload, "sequenceNumber": self.revision,
            "traceId": trace});
        let entry = json!({"id": format!("workspace-file-rewind:rewind_{rewind}"),
            "sessionID": self.id, "type": "runtime/workspace_file_rewind",
            "time": {"created": now, "updated": now}, "data": data});
        self.node.push(now, Op::Entry(entry));
    }
}

/// The checkpoints a stored file rewind restored: from the first checkpoint of
/// its target message through its target checkpoint, in storage order.
pub fn restored_by(checkpoints: &[NodeCheckpoint], rewind: &Value) -> Vec<usize> {
    let payload = &rewind["payload"];
    let target = payload["targetMessageId"].as_str();
    let last = payload["targetCheckpointId"].as_str();
    let Some(start) = checkpoints
        .iter()
        .position(|c| Some(c.message.as_str()) == target)
    else {
        return vec![];
    };
    let end = checkpoints
        .iter()
        .position(|c| Some(c.checkpoint.as_str()) == last)
        .unwrap_or(checkpoints.len() - 1);
    (start..=end.max(start)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkpoints_follow_nodes_artifact() {
        let hunk = json!({"oldStart": 1, "oldLines": 1, "newStart": 1, "newLines": 1,
            "lines": ["-a", "+b"]});
        let output = json!({"type": "update", "filePath": "/w/a.ts", "content": "b\n",
            "originalFile": "a\n", "structuredPatch": [hunk]});
        let write = candidate(&output).unwrap();
        let text = artifact(&write, ("call_1", "Write"), 0);
        assert!(text.starts_with("{\n  \"createdAt\": \"1970-01-01T00:00:00.000Z\",\n  \"files\": [\n    {\n      \"afterContent\": \"b\\n\","));
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["files"][0]["afterContentLength"], 2);
        assert_eq!(parsed["files"][0]["existedBefore"], true);
        let edit = json!({"filePath": "/w/a.ts", "originalFile": null, "structuredPatch": []});
        let file =
            &serde_json::from_str::<Value>(&artifact(&candidate(&edit).unwrap(), ("c", "Edit"), 0))
                .unwrap()["files"][0];
        assert!(file.get("afterContent").is_none() && file["existedBefore"] == false);
        assert!(
            candidate(
                &json!({"filePath": "/w/a.ts", "structuredPatch": [{"lines": []}],
            "originalFile": null})
            )
            .is_none()
        );
        assert!(candidate(&json!({"filePath": "", "structuredPatch": []})).is_none());
    }

    #[test]
    fn a_file_rewind_restores_its_targets_checkpoints() {
        let checkpoint = |id: &str, message: &str| NodeCheckpoint {
            checkpoint: id.into(),
            message: message.into(),
            snapshot: format!("zcode-artifact://s/{id}"),
        };
        let all = [
            checkpoint("c1", "m1"),
            checkpoint("c2", "m2"),
            checkpoint("c3", "m2"),
            checkpoint("c4", "m3"),
        ];
        let mut s = Session::new(
            "s".into(),
            "/w".into(),
            "p".into(),
            "m".into(),
            "".into(),
            "e".into(),
            1,
        );
        s.node.created = true;
        s.node_file_rewind(5, ("r".into(), "e".into(), "t".into()), &all[1..4]);
        let Op::Entry(entry) = &s.node.pending[0].op else {
            panic!("entry");
        };
        assert_eq!(entry["id"], "workspace-file-rewind:rewind_r");
        assert_eq!(
            entry["data"]["payload"],
            json!({"rewindId": "rewind_r", "scope": "workspace", "strategy": "active_chain",
                "targetMessageId": "m2", "targetCheckpointId": "c4",
                "restoredSnapshotRef": "zcode-artifact://s/c4", "reason": "file_summary_rewind"})
        );
        assert_eq!(restored_by(&all, &entry["data"]), [1, 2, 3]);
    }
}
