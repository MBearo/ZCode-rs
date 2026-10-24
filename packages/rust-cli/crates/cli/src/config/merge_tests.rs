//! 三段配置的合并（config.md §2）。
use rust_cli_kernel_api::mcp::McpTransportConfig;
use serde_json::json;

use super::*;

fn object(value: Value) -> Map<String, Value> {
    value.as_object().cloned().unwrap()
}

fn layers<'a>(user: &'a Map<String, Value>, projects: &'a [Map<String, Value>]) -> Vec<Layer<'a>> {
    std::iter::once(Layer {
        source: RuleSource::UserSettings,
        object: user,
        path: "user",
    })
    .chain(projects.iter().map(|object| Layer {
        source: RuleSource::ProjectSettings,
        object,
        path: "project",
    }))
    .collect()
}

#[test]
fn permission_keys_are_replaced_by_the_last_layer_that_sets_them() {
    let user = object(json!({"permission": {
        "mode": "edit", "allowedTools": ["Bash", "Read"], "disallowedTools": ["WebFetch"]
    }}));
    let projects = [
        object(json!({"permission": {"allowedTools": ["Bash(git status:*)"]}})),
        object(json!({"permission": {"mode": "guarded", "disallowedTools": "Write"}})),
    ];
    let mut warnings = Vec::new();
    let merged = permission(&layers(&user, &projects), &mut warnings);
    // 修复线 F20：guarded 是 guarded（以前当 bypassPermissions）。
    assert_eq!(merged.mode, Some(PermissionMode::Guarded));
    // 数组整体替换，不拼接；来源是最后写它的层。
    assert_eq!(
        merged.allowed,
        Some((
            RuleSource::ProjectSettings,
            vec!["Bash(git status:*)".to_owned()]
        ))
    );
    assert_eq!(
        merged.disallowed,
        Some((RuleSource::UserSettings, vec!["WebFetch".to_owned()]))
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
}

#[test]
fn node_modes_map_to_command_line_modes() {
    let modes: Vec<_> = ["build", "edit", "plan", "yolo", "auto", "dontAsk"]
        .into_iter()
        .map(mode_of)
        .collect();
    assert_eq!(
        modes,
        [
            Some(PermissionMode::Default),
            Some(PermissionMode::AcceptEdits),
            Some(PermissionMode::Plan),
            Some(PermissionMode::BypassPermissions),
            Some(PermissionMode::Auto),
            None,
        ]
    );
}

#[test]
fn user_servers_shadow_project_servers_and_closer_projects_win() {
    let user = object(json!({"mcp": {"servers": {
        "shared": {"command": "user"},
        "off": {"command": "user-off", "enabled": false}
    }}}));
    let projects = [
        object(json!({"mcp": {"servers": {
            "shared": {"command": "root"}, "repo": {"command": "root"}, "off": {"command": "project"}
        }}})),
        object(json!({"mcp": {"servers": {"repo": {"command": "closer"}}}})),
    ];
    let mut warnings = Vec::new();
    let servers = mcp_servers(&layers(&user, &projects), &mut warnings);
    let found: Vec<_> = servers
        .iter()
        .map(|server| {
            let McpTransportConfig::Stdio { command, .. } = &server.transport else {
                panic!("stdio expected");
            };
            (server.name.as_str(), command.as_str(), server.scope)
        })
        .collect();
    // 用户级停用的 `off` 也遮住项目级的同名服务器。
    assert_eq!(
        found,
        [
            ("repo", "closer", McpScope::Project),
            ("shared", "user", McpScope::User),
        ]
    );
    assert!(warnings.is_empty());
}

#[test]
fn features_mcp_false_turns_mcp_off() {
    let user = object(json!({"features": {"mcp": false}}));
    assert!(!mcp_enabled(&layers(&user, &[])));
    let projects = [object(json!({"features": {"mcp": true}}))];
    assert!(mcp_enabled(&layers(&user, &projects)));
    assert!(mcp_enabled(&layers(&object(json!({})), &[])));
}

/// `ui.locale`（tui.md §7.1）：按键覆盖，只认三个值，坏值记警告、跳过这一层。
#[test]
fn the_ui_locale_is_taken_from_the_last_layer_with_a_valid_value() {
    let user = object(json!({"ui": {"locale": "zh-CN", "theme": "dark"}}));
    let projects = [
        object(json!({"ui": {"locale": "en-US"}})),
        object(json!({"ui": {"locale": "fr-FR"}})),
        object(json!({"ui": "x"})),
    ];
    let mut warnings = Vec::new();
    assert_eq!(
        locale(&layers(&user, &projects), &mut warnings),
        Some(UiLocale::EnUs)
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(warnings[0].contains("fr-FR"), "{warnings:?}");
    let mut warnings = Vec::new();
    assert_eq!(
        locale(&layers(&object(json!({})), &[]), &mut warnings),
        None
    );
    assert!(warnings.is_empty());
}

/// `ui.theme`（tui-manage.md §2.1）：同 `ui.locale` 的按键覆盖，只认 Node 的三个值。
#[test]
fn the_ui_theme_is_taken_from_the_last_layer_with_a_valid_value() {
    use rust_cli_platform::UiTheme;
    let user = object(json!({"ui": {"theme": "light"}}));
    let projects = [
        object(json!({"ui": {"theme": "dark"}})),
        object(json!({"ui": {"theme": "dark-ansi"}})),
    ];
    let mut warnings = Vec::new();
    assert_eq!(
        theme(&layers(&user, &projects), &mut warnings),
        Some(UiTheme::Dark)
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    let mut warnings = Vec::new();
    assert_eq!(
        theme(&layers(&user, &[]), &mut warnings),
        Some(UiTheme::Light)
    );
}

/// 自动更新的开关（update.md §10，F48）：顶层 `checkForUpdateOnStartup` 按键覆盖，只认布尔，缺省开。
#[test]
fn the_update_check_switch_is_taken_from_the_last_layer_with_a_boolean() {
    let user = object(json!({"checkForUpdateOnStartup": false}));
    let projects = [
        object(json!({"checkForUpdateOnStartup": true})),
        object(json!({"checkForUpdateOnStartup": "no"})),
    ];
    let mut warnings = Vec::new();
    assert!(check_for_update(&layers(&user, &projects), &mut warnings));
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("checkForUpdateOnStartup"),
        "{warnings:?}"
    );
    let mut warnings = Vec::new();
    assert!(!check_for_update(&layers(&user, &[]), &mut warnings));
    let empty = object(json!({"ui": {"checkForUpdateOnStartup": false}}));
    assert!(check_for_update(&layers(&empty, &[]), &mut warnings));
    assert!(warnings.is_empty());
}

/// 提问框的空闲超时（tools-more.md §1.1，工作线 F72）：顶层 `askUserQuestionTimeout` 按键覆盖，只认四个值，缺省不计时。
#[test]
fn the_question_timeout_is_taken_from_the_last_layer_with_a_known_value() {
    let user = object(json!({"askUserQuestionTimeout": "5m"}));
    let projects = [
        object(json!({"askUserQuestionTimeout": "60s"})),
        object(json!({"askUserQuestionTimeout": "30s"})),
    ];
    let mut warnings = Vec::new();
    assert_eq!(
        question_timeout(&layers(&user, &projects), &mut warnings),
        Some(60_000)
    );
    assert_eq!(warnings.len(), 1, "{warnings:?}");
    assert!(
        warnings[0].contains("askUserQuestionTimeout"),
        "{warnings:?}"
    );
    let mut warnings = Vec::new();
    assert_eq!(
        question_timeout(&layers(&user, &[]), &mut warnings),
        Some(300_000)
    );
    let never = [object(json!({"askUserQuestionTimeout": "never"}))];
    assert_eq!(
        question_timeout(&layers(&user, &never), &mut warnings),
        None
    );
    let ten = object(json!({"askUserQuestionTimeout": "10m"}));
    assert_eq!(
        question_timeout(&layers(&ten, &[]), &mut warnings),
        Some(600_000)
    );
    let empty = object(json!({}));
    assert_eq!(question_timeout(&layers(&empty, &[]), &mut warnings), None);
    assert!(warnings.is_empty(), "{warnings:?}");
}

/// 停用的服务器与来源文件（`/mcp enable/disable`，tui-manage.md §6.1）。
#[test]
fn disabled_servers_and_origins_follow_the_shadowing() {
    let user =
        object(json!({"mcp": {"servers": {"off": {"command": "user-off", "enabled": false}}}}));
    let projects = [
        object(json!({"mcp": {"servers": {"repo": {"command": "root"}, "off": {"command": "p"}}}})),
        object(json!({"mcp": {"servers": {"repo": {"command": "closer"}}}})),
    ];
    let mut warnings = Vec::new();
    let found = mcp_layers(&layers(&user, &projects), &mut warnings);
    let names = |servers: &[McpServer]| servers.iter().map(|s| s.name.clone()).collect::<Vec<_>>();
    assert_eq!(names(&found.servers), ["repo"]);
    assert_eq!(names(&found.disabled), ["off"]);
    assert!(found.origins["off"].user);
    assert!(!found.origins["repo"].user);
    // 离工作区根最近的项目文件。
    assert_eq!(found.origins["repo"].path, layers(&user, &projects)[2].path);
}
