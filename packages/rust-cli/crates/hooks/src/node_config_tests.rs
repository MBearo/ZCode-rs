//! Node 形状的 hooks 段（hooks.md §2.2）。
use serde_json::json;

use super::*;

fn parse(value: Value) -> (NodeHooks, Vec<String>) {
    NodeHooks::from_json(&value)
}

#[test]
fn command_and_process_hooks_follow_the_node_shape() {
    let (hooks, warnings) = parse(json!({
        "enabled": true,
        "timeoutMs": 5000,
        "events": {
            "PreToolUse": [{"matcher": "Bash", "hooks": [
                {"type": "command", "command": "guard.sh", "timeout": 2, "async": true, "shell": true},
                {"type": "process", "command": "node", "args": ["check.js", "--x"], "enabled": false}
            ]}],
            "Notification": [{"hooks": [{"type": "process", "command": "notify"}]}]
        }
    }));
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(hooks.enabled, Some(true));
    let pre = hooks.matchers(HookEventName::PreToolUse);
    assert_eq!(pre[0].matcher.as_deref(), Some("Bash"));
    assert_eq!(pre[0].hooks[0].resolved_timeout_ms(5000.0), 2000.0);
    assert_eq!(pre[0].hooks[1].resolved_timeout_ms(5000.0), 5000.0);
    assert_eq!(pre[0].hooks[1].display(), "node check.js --x");
    // 设置文件的其他事件名也接受（Node 只认 7 个）。
    assert_eq!(hooks.matchers(HookEventName::Notification).len(), 1);
}

#[test]
fn user_hooks_run_only_when_the_user_section_is_enabled() {
    let events = json!({"Stop": [{"hooks": [
        {"type": "command", "command": "a"},
        {"type": "command", "command": "b", "enabled": false},
        {"type": "process", "command": "c", "args": ["1"], "timeoutMs": 1500}
    ]}]});
    let (off, _) = parse(json!({"events": events}));
    assert!(off.user_settings().0.hooks.is_empty());
    let (on, _) = parse(json!({"enabled": true, "timeoutMs": 3000, "events": events}));
    let (settings, warnings) = on.user_settings();
    assert!(warnings.is_empty());
    let (event, matchers) = &settings.hooks[0];
    assert_eq!(*event, HookEventName::Stop);
    let commands: Vec<_> = matchers[0]
        .hooks
        .iter()
        .map(|hook| match hook {
            HookCommand::Command(command) => (
                command.command.clone(),
                command.args.clone(),
                command.timeout,
            ),
            other => panic!("unexpected {other:?}"),
        })
        .collect();
    assert_eq!(
        commands,
        vec![
            ("a".to_owned(), None, Some(3.0)),
            ("c".to_owned(), Some(vec!["1".to_owned()]), Some(1.5)),
        ]
    );
}

#[test]
fn bad_items_are_skipped_with_warnings() {
    let (hooks, warnings) = parse(json!({
        "enabled": "yes",
        "timeoutMs": -1,
        "events": {
            "NoSuchEvent": [],
            "Stop": [
                {"matcher": "", "hooks": [{"type": "command", "command": "x"}]},
                {"hooks": [{"type": "prompt", "prompt": "p"}, {"type": "command"}, {"type": "process", "command": "ok", "args": [1]}]},
                {"hooks": [{"type": "command", "command": "kept", "shell": "fish"}]}
            ]
        }
    }));
    assert_eq!(hooks.enabled, None);
    assert_eq!(hooks.timeout_ms, None);
    let stop = hooks.matchers(HookEventName::Stop);
    assert_eq!(stop.len(), 1);
    assert_eq!(stop[0].hooks[0].command, "kept");
    assert_eq!(warnings.len(), 7, "{warnings:?}");
    let mut more = Vec::new();
    let converted = stop[0].hooks[0].to_command(DEFAULT_TIMEOUT_MS, &mut more);
    assert!(matches!(
        converted,
        HookCommand::Command(CommandHook {
            shell: HookShell::Bash,
            ..
        })
    ));
    assert_eq!(more.len(), 1);
}

#[test]
fn timeouts_round_and_stay_positive() {
    let hook = NodeHook {
        command: "x".to_owned(),
        kind: NodeHookKind::Command {
            shell: NodeShell::Unset,
            background: false,
            timeout: Some(0.0001),
        },
        enabled: None,
        timeout_ms: None,
        status_message: None,
        deny_on_error: false,
    };
    assert_eq!(hook.resolved_timeout_ms(60_000.0), 1.0);
    let rounded = NodeHook {
        timeout_ms: Some(1234.5),
        ..hook
    };
    assert_eq!(rounded.resolved_timeout_ms(60_000.0), 1235.0);
}
