//! A tool result over its budget, stored as a session artifact (Node
//! `persistToolResult`).
use anyhow::Result;
use std::path::Path;

/// Node `sanitizePathSegment` + `<toolCallId>-tool-result-<uuid>.json` under `dir`.
pub(super) async fn persist(dir: &Path, call_id: &str, content: &str) -> Result<String> {
    let call: String = call_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "._-".contains(c) {
                c
            } else {
                '_'
            }
        })
        .take(120)
        .collect();
    let call = if call.is_empty() {
        "unknown".into()
    } else {
        call
    };
    tokio::fs::create_dir_all(dir).await?;
    let path = dir.join(format!("{call}-tool-result-{}.json", uuid::Uuid::new_v4()));
    tokio::fs::write(&path, content).await?;
    Ok(path.to_string_lossy().into_owned())
}
