//! The workspace checkpoints of a stored session (Node
//! `restoreWorkspaceCheckpointEntries`): each `runtime/workspace_checkpoint`
//! entry with its before-change artifact, restored when a later
//! `runtime/workspace_file_rewind` undid it. Spec rust-m11-node-storage §5.5.
use super::entries;
use crate::domain::file_checkpoint::{ImportedCheckpoint, NodeCheckpoint, apply_patch};
use crate::domain::node_journal::checkpoint::restored_by;
use anyhow::Result;
use rusqlite::Connection;
use serde_json::Value;

type ArtifactReader<'a> = &'a dyn Fn(&str) -> Option<String>;

/// One stored checkpoint whose artifact is readable and whose content after
/// the change is known (given, or the before content patched).
fn imported(entry: &Value, artifacts: ArtifactReader) -> Option<ImportedCheckpoint> {
    let payload = &entry["payload"];
    let snapshot = payload["snapshotRef"].as_str()?;
    let artifact: Value = serde_json::from_str(&artifacts(snapshot)?).ok()?;
    if artifact["kind"] != "workspace_file_before_change" {
        return None;
    }
    let file = &artifact["files"][0];
    let before = file["beforeContent"].as_str().map(str::to_owned);
    let after = match file["afterContent"].as_str() {
        Some(after) => after.to_owned(),
        None => apply_patch(
            before.as_deref().unwrap_or(""),
            file["structuredPatch"].as_array()?,
        )?,
    };
    Some(ImportedCheckpoint {
        node: NodeCheckpoint {
            checkpoint: payload["checkpointId"].as_str()?.into(),
            message: payload["targetMessageId"]
                .as_str()
                .or(payload["messageId"].as_str())?
                .into(),
            snapshot: snapshot.into(),
        },
        call: artifact["toolCallId"].as_str()?.into(),
        tool: artifact["toolName"].as_str()?.into(),
        path: file["path"].as_str()?.into(),
        before,
        after,
        restored: false,
    })
}

pub fn read(
    conn: &Connection,
    session: &str,
    artifacts: ArtifactReader,
) -> Result<Vec<ImportedCheckpoint>> {
    let mut out: Vec<ImportedCheckpoint> =
        entries::list(conn, session, Some("runtime/workspace_checkpoint"))?
            .iter()
            .filter(|e| e.data["payload"]["scope"] != "conversation")
            .filter_map(|e| imported(&e.data, artifacts))
            .collect();
    let recorded: Vec<NodeCheckpoint> = out.iter().map(|c| c.node.clone()).collect();
    for rewind in entries::list(conn, session, Some("runtime/workspace_file_rewind"))? {
        for index in restored_by(&recorded, &rewind.data) {
            out[index].restored = true;
        }
    }
    Ok(out)
}
