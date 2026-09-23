use super::Engine;
use crate::{
    contract::ToolPort,
    domain::{file_checkpoint::FileCheckpoint, session::Session},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

fn changes(s: &Session, turn: &Value) -> Vec<FileCheckpoint> {
    let start = s.rows.iter().position(|r| r["turnId"] == *turn);
    let Some(start) = start else {
        return vec![];
    };
    let first = s.rows[start]["rowId"].as_u64().unwrap_or(0);
    let end = s.rows[start + 1..]
        .iter()
        .find(|r| r["kind"] == "turnHeader")
        .and_then(|r| r["rowId"].as_u64())
        .unwrap_or(u64::MAX);
    s.file_checkpoints
        .iter()
        .filter(|c| c.row >= first && c.row < end)
        .cloned()
        .collect()
}

// 仅在工具轮结束或缺失摘要的冷加载时读取 blob；流式文本不触发 diff IO。
pub(super) async fn hydrate(
    s: &mut Session,
    tools: &dyn ToolPort,
    turn: Option<&str>,
) -> Result<Vec<Value>> {
    if s.file_checkpoints.is_empty() {
        return Ok(vec![]);
    }
    let targets = s
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| {
            r["kind"] == "turnHeader"
                && match turn {
                    Some(t) => r["turnId"] == t,
                    None => r.get("fileChanges").is_none(),
                }
        })
        .map(|(i, r)| (i, r["turnId"].clone()))
        .collect::<Vec<_>>();
    let mut deltas = vec![];
    for (i, turn) in targets {
        let changes = changes(s, &turn);
        if changes.is_empty() {
            continue;
        }
        let mut summary = tools.file_changes(&changes).await?;
        summary.as_object_mut().unwrap().remove("items");
        let row = &mut s.rows[i];
        row["fileChanges"] = summary;
        if row.get("actions").is_none() {
            row["actions"] = json!({});
        }
        if changes.iter().any(|c| !c.restored) {
            row["actions"]["canRewindFiles"] = true.into();
        } else {
            row["actions"]
                .as_object_mut()
                .unwrap()
                .remove("canRewindFiles");
        }
        s.saved_rows = s.saved_rows.min(i);
        deltas.push(json!({"op":"row.upserted","row":row}));
    }
    Ok(deltas)
}

pub(super) fn restored(s: &mut Session) -> Vec<Value> {
    let turns = s
        .rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r["kind"] == "turnHeader" && r.get("fileChanges").is_some())
        .map(|(i, r)| (i, r["turnId"].clone()))
        .collect::<Vec<_>>();
    let mut deltas = vec![];
    for (i, turn) in turns {
        let changes = changes(s, &turn);
        if !changes.is_empty() && changes.iter().all(|c| c.restored) {
            let row = &mut s.rows[i];
            row["fileChanges"]["state"] = "reverted".into();
            if let Some(actions) = row["actions"].as_object_mut() {
                actions.remove("canRewindFiles");
            }
            s.saved_rows = s.saved_rows.min(i);
            deltas.push(json!({"op":"row.upserted","row":row}));
        }
    }
    deltas
}

impl Engine {
    pub(super) async fn file_changes(&mut self, p: &Value) -> Result<Value> {
        ensure!(
            p.as_object().is_some_and(|o| o.len() == 4),
            "Invalid file changes parameters"
        );
        let id = p["sessionId"].as_str().context("Session required")?;
        self.ensure_session(id).await?;
        let s = &self.sessions[id];
        ensure!(
            p["baseRevision"] == s.revision && p["baseLogEpoch"] == s.epoch,
            "proto.staleRevision"
        );
        let row = s
            .rows
            .iter()
            .find(|r| {
                r["rowId"] == p["target"]["rowId"] && r["entityId"] == p["target"]["entityId"]
            })
            .context("File changes target unavailable")?;
        self.tools.file_changes(&changes(s, &row["turnId"])).await
    }
}
