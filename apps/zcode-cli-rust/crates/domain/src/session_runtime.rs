//! Facts a legacy `session/create` or `session/resume` gives one runtime
//! activation. Node keeps them on the in-memory record and never persists them.
use serde::{Deserialize, Serialize};

/// Legacy `persistence`: whether a session is visible before its first input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Persistence {
    #[default]
    Immediate,
    Deferred,
}

#[derive(Clone, Debug, Default)]
pub struct RuntimeOptions {
    /// Set by legacy create; `None` for V4-created drafts (which behave as deferred).
    pub persistence: Option<Persistence>,
    pub tools: ToolFilter,
    /// `titleGenerationEnabled: false` disables generated titles.
    pub title_generation_disabled: bool,
    /// Legacy `runtime.stateRevision`: model and thought-level mutations of this activation.
    pub state_revision: u64,
    /// Workspace ref echoed by legacy snapshots (the create / resume params).
    pub workspace: Option<serde_json::Value>,
    /// No runtime event yet (Node reducer defaults in the legacy snapshot).
    pub fresh: bool,
    /// Reminders the model has seen in this process (Node in-memory history
    /// attachments): `(session messages before it, kind, message)`.
    pub reminders: Vec<(usize, ReminderKind, serde_json::Value)>,
    /// ExitPlanMode feedback steered into the turn at the next step boundary.
    pub plan_feedback: Option<String>,
    /// SessionStart hooks already ran in this process (Node `sessionStartHookRan`).
    pub session_start_ran: bool,
    /// Snapshot `workspaceHookAdmission`: the pending project hooks banner.
    pub workspace_hook_admission: Option<serde_json::Value>,
    /// Legacy `session/event` subscription of this activation.
    pub legacy: crate::legacy_stream::LegacyStream,
    /// `session/debug` observation of this activation's model requests.
    pub debug: crate::session_debug::DebugLog,
}

/// A transient reminder kept in the process history (never persisted).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReminderKind {
    /// Plan-mode `runtime_mode` reminder (counts for the cadence).
    PlanRuntime,
    PlanExit,
    /// SessionStart / UserPromptSubmit / Stop hook context.
    HookContext,
    /// Repeated tool call or tool call budget reminder (Node `model_anomaly`).
    ModelAnomaly,
}

/// Legacy `toolAllowlist` / `toolDenylist`: tools registered for the session's
/// runtime (Node registration filter, inherited by subagents).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToolFilter {
    /// `None` = every tool; names are alias-normalized (`web_search` = `WebSearch`).
    pub allowlist: Option<Vec<String>>,
    pub denylist: Vec<String>,
}

impl ToolFilter {
    pub fn new(allowlist: Option<Vec<String>>, denylist: Vec<String>) -> Self {
        let alias = |n: String| {
            if n == "web_search" {
                "WebSearch".into()
            } else {
                n
            }
        };
        Self {
            allowlist: allowlist.map(|a| a.into_iter().map(alias).collect()),
            denylist,
        }
    }

    /// MCP tools (`mcp__<server>__<tool>`) also match by their declared name.
    pub fn allows(&self, name: &str) -> bool {
        let declared = name
            .strip_prefix("mcp__")
            .and_then(|rest| rest.split_once("__"))
            .map(|(_, tool)| tool);
        let hit = |list: &[String]| {
            list.iter()
                .any(|n| n == name || Some(n.as_str()) == declared)
        };
        self.allowlist.as_deref().is_none_or(hit) && !hit(&self.denylist)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_then_denylist_with_aliases_and_mcp_names() {
        let filter = ToolFilter::new(
            Some(vec![
                "Read".into(),
                "Bash".into(),
                "web_search".into(),
                "lookup".into(),
            ]),
            vec!["Bash".into()],
        );
        assert!(filter.allows("Read") && filter.allows("WebSearch"));
        assert!(!filter.allows("Bash") && !filter.allows("Write"));
        assert!(filter.allows("mcp__docs__lookup") && !filter.allows("mcp__docs__other"));
        assert!(ToolFilter::default().allows("Write"));
    }
}
