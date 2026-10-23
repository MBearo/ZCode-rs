//! 插件的 hooks（hooks.md §2.2 第 5–7 条）。

use rust_cli_kernel_api::hooks::HookEventName;
use serde_json::{Value, json};

use super::*;
use crate::config::{HookCommand, HookSource, HooksConfig, HooksSettings};

fn stop_file(command: &str) -> Value {
    json!({"description": "kit hooks", "hooks": {"Stop": [
        {"hooks": [{"type": "command", "command": command}]},
        {"matcher": "x", "hooks": []}
    ]}})
}

fn settings(value: Value) -> HooksSettings {
    HooksSettings::from_json(&value).0
}

fn stop_commands(config: &HooksConfig, plugins: &[PluginHooks]) -> Vec<(String, Option<String>)> {
    config
        .effective_with(plugins)
        .matchers
        .get(&HookEventName::Stop)
        .into_iter()
        .flatten()
        .flat_map(|matcher| matcher.hooks.iter())
        .map(|hook| match hook {
            HookCommand::Command(command) => (command.command.clone(), command.plugin_root.clone()),
            other => (other.display(), None),
        })
        .collect()
}

fn plugin(name: &str, root: &str, command: &str) -> PluginHooks {
    let (plugin, warnings) = PluginHooks::from_json(name, root, &[stop_file(command)]);
    assert!(warnings.is_empty(), "{warnings:?}");
    plugin
}

#[test]
fn plugin_matchers_follow_the_settings_and_carry_their_root() {
    let kit = plugin(
        "kit",
        "/Users/dev/plugins/kit",
        "${CLAUDE_PLUGIN_ROOT}/stop.sh",
    );
    // 空的 matcher 跳过。
    assert_eq!(kit.settings.hooks[0].1.len(), 1);
    let config = HooksConfig {
        sources: vec![(
            HookSource::User,
            settings(
                json!({"hooks": {"Stop": [{"hooks": [{"type": "command", "command": "user.sh"}]}]}}),
            ),
        )],
        ..HooksConfig::default()
    };
    assert_eq!(
        stop_commands(&config, &[kit]),
        [
            ("user.sh".to_owned(), None),
            (
                "${CLAUDE_PLUGIN_ROOT}/stop.sh".to_owned(),
                Some("/Users/dev/plugins/kit".to_owned())
            ),
        ]
    );
}

#[test]
fn managed_only_and_policy_disable_drop_plugin_hooks() {
    let kit = plugin("kit", "/Users/dev/plugins/kit", "kit.sh");
    let with = |source: HookSource, value: Value| HooksConfig {
        sources: vec![(source, settings(value))],
        ..HooksConfig::default()
    };
    let only = |config: &HooksConfig| stop_commands(config, std::slice::from_ref(&kit)).len();
    assert_eq!(only(&HooksConfig::default()), 1);
    assert_eq!(
        only(&with(
            HookSource::Policy,
            json!({"allowManagedHooksOnly": true})
        )),
        0
    );
    assert_eq!(
        only(&with(HookSource::Project, json!({"disableAllHooks": true}))),
        0
    );
    assert_eq!(
        only(&with(HookSource::Policy, json!({"disableAllHooks": true}))),
        0
    );
}

#[test]
fn the_same_command_in_two_plugins_is_not_deduplicated() {
    let config = HooksConfig::default();
    let plugins = [
        plugin("a", "/Users/dev/plugins/a", "${CLAUDE_PLUGIN_ROOT}/hook.sh"),
        plugin("b", "/Users/dev/plugins/b", "${CLAUDE_PLUGIN_ROOT}/hook.sh"),
        plugin(
            "a2",
            "/Users/dev/plugins/a",
            "${CLAUDE_PLUGIN_ROOT}/hook.sh",
        ),
    ];
    let effective = config.effective_with(&plugins);
    let hooks = crate::matcher::select(
        HookEventName::Stop,
        &effective.matchers[&HookEventName::Stop],
        &json!({}),
    );
    // 同一插件目录的重复项去掉，不同插件目录的保留。
    assert_eq!(hooks.len(), 2);
}
