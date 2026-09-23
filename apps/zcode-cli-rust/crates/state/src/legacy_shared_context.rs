use crate::domain::{
    session::Session,
    shared_context::{Provenance, SharedContext, Status},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::path::Path;

pub(super) fn project(
    session: &mut Session,
    message: &Value,
    parts: &[(String, Value)],
    entries: &[(String, Value)],
    dir: &Path,
) -> Result<bool> {
    if message["role"] != "user" || message["source"] != "shared_context" {
        return Ok(false);
    }
    ensure!(
        session.shared_context.is_none() && !session.legacy_shared_context,
        "Multiple legacy shared contexts are unsupported"
    );
    let text = parts
        .iter()
        .filter(|(_, p)| p["type"] == "text")
        .filter_map(|(_, p)| p["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n");
    ensure!(
        !text.trim().is_empty(),
        "Legacy shared context content is missing"
    );
    let status = message["metadata"].get("sharedContextStatus");
    let entry = entries
        .iter()
        .find(|(kind, _)| kind == "v4/shared_context_import");
    if let Some((_, data)) = entry {
        let mut data = data.clone();
        let source_id = data["sourceId"].as_str().map(str::to_owned);
        let attached_message_id = data["attachedMessageId"].as_str().map(str::to_owned);
        if let Some(object) = data.as_object_mut() {
            object.remove("sourceId");
            object.remove("attachedMessageId");
        }
        // 与 TS hydrator 一致，无生命周期状态的早期上下文已经属于模型历史。
        if data.get("status").is_none() {
            data["status"] = status.cloned().unwrap_or_else(|| json!("attached"));
        }
        let mut provenance: Provenance = serde_json::from_value(data)?;
        provenance.validate(&session.id, &text)?;
        if let Some(id) = message["metadata"].get("contextId") {
            ensure!(
                id.as_str() == provenance.context_id.as_deref(),
                "Legacy shared context identity mismatch"
            );
        }
        if let Some(status) = status {
            ensure!(
                *status == serde_json::to_value(provenance.status)?,
                "Legacy shared context status mismatch"
            );
        }
        let content =
            super::legacy_attachments::snapshot_bytes("text/markdown", text.as_bytes(), dir)?;
        if provenance.status == Status::Attached {
            session.append_message(json!({"role":"user","content":text}));
        }
        session.shared_context = Some(SharedContext {
            provenance,
            content,
            source_id,
            attached_message_id,
        });
    } else {
        ensure!(
            status.is_none(),
            "Legacy shared context provenance is missing"
        );
        session.legacy_shared_context = true;
        session.append_message(json!({"role":"user","content":text}));
    }
    ensure!(
        !session.title.trim().is_empty(),
        "Legacy shared context title is missing"
    );
    Ok(true)
}

pub(super) fn validate(session: &Session, entries: &[(String, Value)]) -> Result<()> {
    if entries
        .iter()
        .any(|(kind, _)| kind == "v4/shared_context_import")
    {
        session
            .shared_context
            .as_ref()
            .context("Legacy shared context message is missing")?;
    }
    Ok(())
}
