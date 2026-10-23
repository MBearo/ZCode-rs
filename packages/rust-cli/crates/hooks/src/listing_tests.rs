//! `/hooks` 的快照（tui-display.md §3.5）：来源顺序、matcher 与显示文本。
use serde_json::json;

use super::*;

#[test]
fn hooks_are_listed_by_source_event_and_matcher() {
    let (user, _) = HooksSettings::from_json(&json!({"hooks": {
        "PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "echo pre"}]}],
        "Stop": [{"hooks": [{"type": "prompt", "prompt": "Check the goal"}]}],
    }}));
    let (project, _) = HooksSettings::from_json(&json!({"hooks": {
        "PostToolUse": [{"matcher": "Edit|Write", "hooks": [{"type": "http", "url": "https://hooks.example.com/post"}]}],
    }}));
    let plugin = PluginHooks {
        plugin: "lint".to_owned(),
        root: "/Users/dev/.zcode/plugins/lint".to_owned(),
        settings: HooksSettings::from_json(&json!({"hooks": {
            "SessionStart": [{"hooks": [{"type": "command", "command": "lint --init"}]}],
        }}))
        .0,
    };
    let listed = entries(
        &[(HookSource::User, user), (HookSource::Project, project)],
        &[plugin],
    );
    let rows: Vec<(&str, Option<&str>, &str, &str, &str)> = listed
        .iter()
        .map(|e| {
            (
                e.event.as_str(),
                e.matcher.as_deref(),
                e.kind.as_str(),
                e.display.as_str(),
                e.source.as_str(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            ("PreToolUse", Some("Bash"), "command", "echo pre", "user"),
            ("Stop", None, "prompt", "Check the goal", "user"),
            (
                "PostToolUse",
                Some("Edit|Write"),
                "http",
                "https://hooks.example.com/post",
                "project"
            ),
            (
                "SessionStart",
                None,
                "command",
                "lint --init",
                "plugin:lint"
            ),
        ]
    );
}
