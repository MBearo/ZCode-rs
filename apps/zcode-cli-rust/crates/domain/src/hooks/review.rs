//! The review request shown to the user (Node
//! `workspace-hook-review-request.ts`).
use super::trust::Item;
use super::workspace::Snapshot;
use serde_json::{Value, json};
use std::collections::BTreeSet;
use std::path::Path;

/// Where a review is shown (Node `WorkspaceHookReviewHostContext`).
pub struct ReviewHost<'a> {
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub workspace_label: &'a str,
    pub remote_session_id: Option<&'a str>,
}

/// Node `WORKSPACE_HOOK_REVIEW_TIMEOUT_MS`.
pub const REVIEW_TIMEOUT_MS: u64 = 600_000;

/// Node `buildWorkspaceHookReviewRequest`.
pub fn review_request(
    snapshot: &Snapshot,
    items: &[Item],
    flow: (&str, u64, &str),
    host: &ReviewHost,
    now: u64,
) -> Value {
    let (flow_id, generation, interaction_id) = flow;
    let rows: Vec<Value> = snapshot
        .hooks
        .iter()
        .map(|entry| {
            let state = items
                .iter()
                .find(|i| i.review_item_id == entry.review_item_id)
                .map(|i| i.trust_state.as_str())
                .unwrap_or("blocked_untrusted");
            let mut row =
                json!({"reviewItemId": entry.review_item_id, "event": entry.event.as_str()});
            if let Some(matcher) = entry.matcher.as_ref().filter(|m| !m.is_empty()) {
                row["matcher"] = matcher.clone().into();
            }
            let name = match entry.matcher.as_ref().filter(|m| !m.is_empty()) {
                Some(m) => format!("{} · {m}", entry.event.as_str()),
                None => entry.event.as_str().into(),
            };
            let background = matches!(
                entry.program,
                super::Program::Command {
                    background: true,
                    ..
                }
            );
            row["type"] = entry.kind().into();
            row["displayName"] = name.into();
            row["displayCommand"] = entry.display_command().trim().into();
            row["sourcePath"] = entry.source_relative_path.clone().into();
            row["resolvedTimeoutMs"] = entry.resolved_timeout_ms.into();
            row["resolvedMaxOutputBytes"] = entry.resolved_max_output_bytes.into();
            row["executionMode"] = if background {
                "background"
            } else {
                "foreground"
            }
            .into();
            row["configuredEnabled"] = entry.configured_enabled.into();
            row["editable"] = entry.editable.into();
            row["trustState"] = state.into();
            row
        })
        .collect();
    let events: BTreeSet<&str> = snapshot.hooks.iter().map(|e| e.event.as_str()).collect();
    let pending = rows
        .iter()
        .filter(|r| {
            matches!(
                r["trustState"].as_str(),
                Some("pending_trust" | "revoked" | "stale_digest")
            )
        })
        .count();
    let sources: Vec<Value> = snapshot
        .source_files
        .iter()
        .map(|s| {
            let relative = Path::new(&s.canonical_path)
                .strip_prefix(&s.base_dir)
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let display = if relative.is_empty() {
                Path::new(&s.canonical_path)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default()
            } else {
                relative
            };
            json!({"path": s.canonical_path, "displayPath": display, "editable": s.editable})
        })
        .collect();
    let mut request = json!({
        "kind": "workspaceHookReview",
        "reviewFlowId": flow_id,
        "generation": generation,
        "interactionId": interaction_id,
        "sessionId": host.session_id,
        "taskId": host.session_id,
        "runId": host.run_id,
        "workspaceIdentity": snapshot.workspace_identity,
        "workspaceLabel": host.workspace_label,
        "bundleDigest": snapshot.bundle_digest,
        "createdAt": now,
        "deadlineAt": now + REVIEW_TIMEOUT_MS,
        "sourceFiles": sources,
        "summary": {"eventCount": events.len(), "hookCount": rows.len(), "pendingCount": pending},
        "items": rows,
        "warningCode": "workspace_hooks_execute_code",
    });
    if let Some(remote) = host.remote_session_id {
        request["remoteSessionId"] = remote.into();
    }
    request
}
