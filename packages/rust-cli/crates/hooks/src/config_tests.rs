//! 设置解析与来源合并（spec §2）。

use rust_cli_kernel_api::hooks::HookEventName;
use serde_json::json;

use super::*;
use crate::effective::Effective;

fn settings(value: Value) -> HooksSettings {
    let (parsed, warnings) = HooksSettings::from_json(&value);
    assert!(warnings.is_empty(), "{warnings:?}");
    parsed
}

fn command(text: &str) -> Value {
    json!({"type": "command", "command": text})
}

fn commands(effective: &Effective, event: HookEventName) -> Vec<String> {
    effective
        .matchers
        .get(&event)
        .into_iter()
        .flatten()
        .flat_map(|matcher| matcher.hooks.iter().map(HookCommand::display))
        .collect()
}

#[test]
fn parses_every_hook_type_and_field() {
    let parsed = settings(json!({
        "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [
            {"type": "command", "command": "node", "args": ["a.js"], "shell": "powershell",
             "timeout": 5, "if": "Bash(git *)", "async": true},
            {"type": "prompt", "prompt": "ok? $ARGUMENTS", "model": "m", "continueOnBlock": true},
            {"type": "agent", "prompt": "verify"},
            {"type": "http", "url": "https://hooks.example.com/h", "headers": {"X-T": "$T"},
             "allowedEnvVars": ["T"], "timeout": 1.5}
        ]}]},
        "disableAllHooks": false,
        "allowedHttpHookUrls": ["https://hooks.example.com/*"],
        "httpHookAllowedEnvVars": ["T"]
    }));
    let (event, matchers) = &parsed.hooks[0];
    assert_eq!(*event, HookEventName::PreToolUse);
    assert_eq!(matchers[0].matcher.as_deref(), Some("Bash"));
    let hooks = &matchers[0].hooks;
    assert_eq!(
        hooks[0],
        HookCommand::Command(CommandHook {
            command: "node".to_owned(),
            args: Some(vec!["a.js".to_owned()]),
            shell: HookShell::PowerShell,
            timeout: Some(5.0),
            condition: Some("Bash(git *)".to_owned()),
            background: true,
            plugin_root: None,
        })
    );
    assert_eq!(hooks[0].display(), "node a.js");
    assert!(
        matches!(&hooks[1], HookCommand::Prompt(p) if p.continue_on_block && p.model.as_deref() == Some("m"))
    );
    assert!(matches!(&hooks[2], HookCommand::Agent(a) if a.prompt == "verify"));
    assert!(matches!(&hooks[3], HookCommand::Http(h)
        if h.headers == vec![("X-T".to_owned(), "$T".to_owned())] && h.timeout == Some(1.5)));
    assert_eq!(parsed.disable_all_hooks, Some(false));
    assert_eq!(
        parsed
            .allowed_http_hook_urls
            .as_deref()
            .map(<[String]>::len),
        Some(1)
    );
}

#[test]
fn bad_entries_are_skipped_with_warnings() {
    let (parsed, warnings) = HooksSettings::from_json(&json!({
        "hooks": {
            "NoSuchEvent": [],
            "Stop": "x",
            "PreToolUse": [
                {"hooks": [{"type": "command"}, {"type": "mcp_tool"}, {"type": "command", "command": "ok", "timeout": 0}, command("good")]},
                {"matcher": "Bash"}
            ]
        }
    }));
    assert_eq!(warnings.len(), 6, "{warnings:?}");
    let matchers = &parsed.hooks[0].1;
    assert_eq!(matchers.len(), 1);
    assert_eq!(matchers[0].hooks.len(), 1);
    assert_eq!(matchers[0].hooks[0].display(), "good");
}

fn source(source: HookSource, value: Value) -> (HookSource, HooksSettings) {
    (source, settings(value))
}

fn stop_hook(text: &str) -> Value {
    json!({"hooks": {"Stop": [{"hooks": [command(text)]}]}})
}

#[test]
fn sources_concatenate_in_merge_order() {
    let config = HooksConfig {
        sources: vec![
            source(HookSource::Policy, stop_hook("policy")),
            source(HookSource::User, stop_hook("user")),
            source(HookSource::Local, stop_hook("local")),
            source(HookSource::Project, stop_hook("project")),
        ],
        ..HooksConfig::default()
    };
    assert_eq!(
        commands(&config.effective(), HookEventName::Stop),
        ["user", "project", "local", "policy"]
    );
}

#[test]
fn disable_and_managed_only_switches_apply() {
    let with = |extra: Vec<(HookSource, HooksSettings)>| {
        let mut sources = vec![
            source(HookSource::User, stop_hook("user")),
            source(HookSource::Policy, stop_hook("policy")),
        ];
        sources.extend(extra);
        commands(
            &HooksConfig {
                sources,
                ..HooksConfig::default()
            }
            .effective(),
            HookEventName::Stop,
        )
    };
    assert_eq!(with(Vec::new()), ["user", "policy"]);
    // 非 managed 的 disableAllHooks：managed hooks 仍运行。
    assert_eq!(
        with(vec![source(
            HookSource::Project,
            json!({"disableAllHooks": true})
        )]),
        ["policy"]
    );
    // 后面的来源覆盖前面的 disableAllHooks。
    assert_eq!(
        with(vec![
            source(HookSource::Project, json!({"disableAllHooks": true})),
            source(HookSource::Local, json!({"disableAllHooks": false})),
        ]),
        ["user", "policy"]
    );
    assert_eq!(
        with(vec![source(
            HookSource::Policy,
            json!({"allowManagedHooksOnly": true})
        )]),
        ["policy"]
    );
    assert!(
        with(vec![source(
            HookSource::Policy,
            json!({"disableAllHooks": true})
        )])
        .is_empty()
    );
}

#[test]
fn http_policies_concatenate_across_sources() {
    let config = HooksConfig {
        sources: vec![
            source(HookSource::User, json!({"allowedHttpHookUrls": ["a"]})),
            source(HookSource::Project, json!({})),
            source(
                HookSource::Policy,
                json!({"allowedHttpHookUrls": ["b"], "httpHookAllowedEnvVars": []}),
            ),
        ],
        ..HooksConfig::default()
    };
    let effective = config.effective();
    assert_eq!(
        effective.allowed_urls,
        Some(vec!["a".to_owned(), "b".to_owned()])
    );
    assert_eq!(effective.allowed_env_vars, Some(Vec::new()));
    assert_eq!(HooksConfig::default().effective().allowed_urls, None);
}

/// 缺陷 R2-13：入口捕获的进程环境原来是明文的 `Vec`，放在 `derive(Debug)` 的快照里，`{:?}` 会打出 API key
/// 等值。现在 Debug 只列变量名。
#[test]
fn debug_output_lists_variable_names_only() {
    let config = HooksConfig {
        env: vec![("ZCODE_API_KEY".to_owned(), "sk-secret-value".to_owned())].into(),
        ..HooksConfig::default()
    };
    let shown = format!("{config:?}");
    assert!(shown.contains("ZCODE_API_KEY"), "{shown}");
    assert!(!shown.contains("sk-secret-value"), "{shown}");
}
