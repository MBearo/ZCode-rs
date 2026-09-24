//! Shared context imports in the Node database (spec rust-m11-node-storage
//! §5.6): Node `transitionSharedContextImport` and the import a resumed
//! session reads back.
use super::entries;
use super::messages::{messages, save_message};
use crate::domain::session::StoredAttachment;
use crate::domain::shared_context::{Provenance, SharedContext};
use anyhow::Result;
use rusqlite::Connection;
use serde_json::{Map, Value};

const ENTRY: &str = "v4/shared_context_import";

/// Node `transitionSharedContextImport`: `false` when the import is missing
/// or not in one of `from`.
pub fn transition(
    conn: &Connection,
    session: &str,
    context: &str,
    (from, to): (&[String], &str),
    source: Option<&str>,
    now: i64,
) -> Result<bool> {
    let entry = entries::list(conn, session, Some(ENTRY))?
        .into_iter()
        .find(|e| e.data["contextId"] == context);
    let Some(mut entry) = entry else {
        return Ok(false);
    };
    if !from.iter().any(|s| entry.data["status"] == s.as_str()) {
        return Ok(false);
    }
    entry.time_updated = now;
    entry.data["status"] = to.into();
    if let Some(source) = source {
        entry.data["sourceId"] = source.into();
    }
    entries::save(conn, &entry)?;
    let found = messages(conn, session)?
        .into_iter()
        .find(|m| m.info["metadata"]["contextId"] == context);
    if let Some(mut found) = found {
        let mut metadata = match found.info.get("metadata") {
            Some(Value::Object(m)) => m.clone(),
            _ => Map::new(),
        };
        metadata.insert("sharedContextStatus".into(), to.into());
        found.info["metadata"] = Value::Object(metadata);
        save_message(conn, &found.info, None, now)?;
    }
    Ok(true)
}

/// The session's import: its provenance and the context message's Markdown.
pub fn read(conn: &Connection, session: &str) -> Result<Option<SharedContext>> {
    let Some(entry) = entries::list(conn, session, Some(ENTRY))?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let mut data = entry.data.clone();
    let context = data["contextId"].clone();
    let (source, attached) = match data.as_object_mut() {
        Some(fields) => (
            fields.remove("sourceId"),
            fields.remove("attachedMessageId"),
        ),
        None => (None, None),
    };
    let Ok(provenance) = serde_json::from_value::<Provenance>(data) else {
        return Ok(None);
    };
    let markdown = messages(conn, session)?
        .into_iter()
        .find(|m| m.info["metadata"]["contextId"] == context)
        .and_then(|m| {
            m.parts
                .iter()
                .find_map(|p| p["text"].as_str().map(str::to_owned))
        });
    Ok(Some(SharedContext {
        provenance,
        content: StoredAttachment::default(),
        markdown,
        source_id: source.and_then(|s| s.as_str().map(str::to_owned)),
        attached_message_id: attached.and_then(|s| s.as_str().map(str::to_owned)),
    }))
}

/// Node `isImportedHistoryMessage`.
fn imported(message: &super::messages::WithParts, source: &str) -> bool {
    let id = message.info["id"].as_str().unwrap_or("");
    let legacy = id
        .strip_prefix("msg_import_")
        .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
    legacy
        || id.starts_with("msg_claude-import-")
        || message.info["metadata"]["migrationSource"] == source
        || message
            .parts
            .iter()
            .any(|p| p["metadata"]["migrationSource"] == source)
}

/// Node `removePreviousImportedSessionHistory`: earlier imports of `source`
/// leave; messages chatted after them stay.
pub fn remove_imported(conn: &Connection, session: &str, source: &str) -> Result<()> {
    for message in messages(conn, session)? {
        if imported(&message, source) {
            let id = message.info["id"].as_str().unwrap_or("").to_owned();
            super::messages::remove_message(conn, session, &id)?;
        }
    }
    Ok(())
}
