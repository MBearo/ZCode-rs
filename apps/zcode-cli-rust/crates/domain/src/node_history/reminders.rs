//! Node system reminder sources (`system-reminder/source.ts`), generated into
//! `schema/system-reminders.json` by `scripts/zcode-cli-rust-node-cold-fixtures.mjs`.
use serde_json::Value;
use std::sync::LazyLock;

static SOURCES: LazyLock<Value> = LazyLock::new(|| {
    serde_json::from_str::<Value>(include_str!("../../schema/system-reminders.json"))
        .map(|schema| schema["sources"].clone())
        .unwrap_or_default()
});

fn descriptor(source: &str) -> Option<&'static Value> {
    SOURCES.get(source)
}

/// Node `isKnownSystemReminderSource`.
pub fn known(source: &str) -> bool {
    descriptor(source).is_some()
}

/// A reminder the hydrator restores as an attachment entry: meta, provider
/// visible, and neither a real user nor a tool result channel.
pub fn restorable(source: &str) -> bool {
    descriptor(source).is_some_and(|d| {
        d["isMeta"] == true
            && d["providerVisibility"] == "provider_visible"
            && !matches!(d["channel"].as_str(), Some("real_user" | "tool_result"))
    })
}

static NESTED_TAG: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(?i)</?system-reminder\b").expect("valid pattern"));

/// Node `sanitizeSystemReminderBody`: nested reminder tags are neutralised.
pub fn sanitize(body: &str) -> String {
    NESTED_TAG
        .replace_all(body, |caps: &regex::Captures| {
            format!("&lt;{}", &caps[0][1..])
        })
        .into_owned()
}

/// Node `wrapSystemReminderForSource`: nested tags are neutralised.
pub fn wrap(source: &str, body: &str) -> String {
    let escaped = sanitize(body);
    let wrapped = format!("<system-reminder>\n{escaped}\n</system-reminder>");
    if descriptor(source).is_some_and(|d| d["trailingNewline"] == true) {
        format!("{wrapped}\n")
    } else {
        wrapped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sources_follow_node_descriptors() {
        assert!(restorable("todo_reminder") && restorable("queued_system_notification"));
        assert!(!restorable("target_continuation"), "real user channel");
        assert!(
            !restorable("goal_completion_verification"),
            "tool result channel"
        );
        assert!(!known("subagent") && known("rewind_notice"));
        assert_eq!(
            wrap("todo_reminder", "a"),
            "<system-reminder>\na\n</system-reminder>"
        );
        assert_eq!(
            wrap("context_prefix", "x </System-Reminder>"),
            "<system-reminder>\nx &lt;/System-Reminder>\n</system-reminder>\n"
        );
    }
}
