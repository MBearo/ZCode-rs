//! 三段配置的合并（docs/specs/rust-cli/config.md §2；Node `config-merger.ts`、`config-factory.ts`）。
//!
//! 输入是按优先级排好的层（用户级在前，项目级自根向工作区根），都已是 JSON 对象；这里不做 IO。

use rust_cli_base::units;
use rust_cli_ext::mcp::{McpScope, McpServer, parse_node_servers};
use rust_cli_kernel_api::permission::{PermissionMode, RuleSource};
use rust_cli_platform::UiLocale;
use serde_json::{Map, Value};

/// 一层配置：来源与对象。
pub(crate) struct Layer<'a> {
    pub(crate) source: RuleSource,
    pub(crate) object: &'a Map<String, Value>,
    /// 警告里标出的文件。
    pub(crate) path: &'a str,
}

/// 合并后的 `permission`：每个键取最后写它的层。
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Permission {
    pub(crate) mode: Option<PermissionMode>,
    pub(crate) allowed: Option<(RuleSource, Vec<String>)>,
    pub(crate) disallowed: Option<(RuleSource, Vec<String>)>,
}

/// Node 的模式值 → 权限模式（config.md §2）。修复线 F20：`guarded` 是 guarded（以前当 bypassPermissions）；
/// `plan` 是单值的 plan（新会话 plan 开、default）。
pub(crate) fn mode_of(value: &str) -> Option<PermissionMode> {
    PermissionMode::from_node_name(value)
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

pub(crate) fn permission(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> Permission {
    let mut merged = Permission::default();
    for layer in layers {
        let Some(section) = layer.object.get("permission") else {
            continue;
        };
        let Some(section) = section.as_object() else {
            warnings.push(format!("{}: permission must be an object", layer.path));
            continue;
        };
        if let Some(mode) = section.get("mode") {
            match mode.as_str().and_then(mode_of) {
                Some(mode) => merged.mode = Some(mode),
                None => warnings.push(format!("{}: unknown permission.mode {mode}", layer.path)),
            }
        }
        for (key, slot) in [
            ("allowedTools", &mut merged.allowed),
            ("disallowedTools", &mut merged.disallowed),
        ] {
            if let Some(value) = section.get(key) {
                match strings(value) {
                    Some(rules) => *slot = Some((layer.source, rules)),
                    None => warnings.push(format!(
                        "{}: permission.{key} must be an array of strings",
                        layer.path
                    )),
                }
            }
        }
    }
    merged
}

/// `ui.locale` 按键覆盖（tui.md §7.1，Node `ui.locale`）：只认 `zh-CN`、`en-US`、`auto`，坏值记警告、
/// 跳过这一层。
pub(crate) fn locale(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> Option<UiLocale> {
    let mut merged = None;
    for layer in layers {
        let Some(value) = layer.object.get("ui").and_then(|ui| ui.get("locale")) else {
            continue;
        };
        match value.as_str().and_then(UiLocale::parse) {
            Some(locale) => merged = Some(locale),
            None => warnings.push(format!("{}: unknown ui.locale {value}", layer.path)),
        }
    }
    merged
}

/// `ui.theme` 按键覆盖（tui-manage.md §2.1，Node `ui.theme`）：只认 `auto`、`dark`、`light`，坏值记警告、跳过这一层。
pub(crate) fn theme(
    layers: &[Layer<'_>],
    warnings: &mut Vec<String>,
) -> Option<rust_cli_platform::UiTheme> {
    let mut merged = None;
    for layer in layers {
        let Some(value) = layer.object.get("ui").and_then(|ui| ui.get("theme")) else {
            continue;
        };
        match value.as_str().and_then(rust_cli_platform::UiTheme::parse) {
            Some(theme) => merged = Some(theme),
            None => warnings.push(format!("{}: unknown ui.theme {value}", layer.path)),
        }
    }
    merged
}

/// 顶层 `checkForUpdateOnStartup` 按键覆盖（自动更新的开关，docs/specs/rust-cli/update.md §10，F48；Codex
/// `check_for_update_on_startup`）：只认布尔，坏值记警告、跳过这一层；没有时开。
pub(crate) fn check_for_update(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> bool {
    let mut merged = true;
    for layer in layers {
        let Some(value) = layer.object.get("checkForUpdateOnStartup") else {
            continue;
        };
        match value.as_bool() {
            Some(enabled) => merged = enabled,
            None => warnings.push(format!(
                "{}: checkForUpdateOnStartup must be a boolean",
                layer.path
            )),
        }
    }
    merged
}

/// `askUserQuestionTimeout` 的三个取值 `60s`、`5m`、`10m` 的毫秒数。
const QUESTION_TIMEOUT_60S_MS: u64 = 60 * units::MS_PER_SECOND;
const QUESTION_TIMEOUT_5M_MS: u64 = 5 * units::MS_PER_MINUTE;
const QUESTION_TIMEOUT_10M_MS: u64 = 10 * units::MS_PER_MINUTE;

/// 顶层 `askUserQuestionTimeout` 按键覆盖（提问框的空闲超时，docs/specs/rust-cli/tools-more.md §1.1，工作线 F72）：只认
/// `60s`、`5m`、`10m`、`never`，坏值记警告、跳过这一层；没有或 `never` 时不计时（None）。返回毫秒。
pub(crate) fn question_timeout(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> Option<u64> {
    let mut merged = None;
    for layer in layers {
        let Some(value) = layer.object.get("askUserQuestionTimeout") else {
            continue;
        };
        match value.as_str() {
            Some("60s") => merged = Some(QUESTION_TIMEOUT_60S_MS),
            Some("5m") => merged = Some(QUESTION_TIMEOUT_5M_MS),
            Some("10m") => merged = Some(QUESTION_TIMEOUT_10M_MS),
            Some("never") => merged = None,
            _ => warnings.push(format!(
                "{}: unknown askUserQuestionTimeout {value}",
                layer.path
            )),
        }
    }
    merged
}

/// `features.mcp` 按键覆盖；最终为 `false` 时关闭 MCP。
pub(crate) fn mcp_enabled(layers: &[Layer<'_>]) -> bool {
    layers
        .iter()
        .rev()
        .find_map(|layer| layer.object.get("features")?.get("mcp")?.as_bool())
        .unwrap_or(true)
}

/// MCP 服务器最终生效的定义所在的配置文件（`/mcp enable/disable` 写这里，tui-manage.md §6.1）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct McpOrigin {
    pub(crate) path: String,
    /// 用户级文件（经 platform 写）；否则是项目级文件（经执行环境写）。
    pub(crate) user: bool,
}

/// 合并后的 `mcp.servers`：启用的、停用的（`enabled: false`），以及每个名字的来源文件。
#[derive(Debug, Clone, Default)]
pub(crate) struct McpLayers {
    pub(crate) servers: Vec<McpServer>,
    pub(crate) disabled: Vec<McpServer>,
    pub(crate) origins: std::collections::BTreeMap<String, McpOrigin>,
}

/// `mcp.servers`：项目级按名合并（后面的覆盖前面的），然后用户级按名覆盖项目级（Node
/// `resolveEffectiveMcpServers`：用户级遮住项目级），最后去掉停用的。结果按名字排序。
#[cfg(test)]
pub(crate) fn mcp_servers(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> Vec<McpServer> {
    mcp_layers(layers, warnings).servers
}

/// 同 [`mcp_servers`]，另给停用的服务器与来源文件（停用的照样遮住低优先级的同名服务器）。
pub(crate) fn mcp_layers(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> McpLayers {
    let mut merged = std::collections::BTreeMap::new();
    let mut origins = std::collections::BTreeMap::new();
    let project = layers
        .iter()
        .filter(|layer| layer.source == RuleSource::ProjectSettings);
    let user = layers
        .iter()
        .filter(|layer| layer.source == RuleSource::UserSettings);
    for layer in project.chain(user) {
        let Some(table) = layer.object.get("mcp").and_then(|mcp| mcp.get("servers")) else {
            continue;
        };
        let scope = match layer.source {
            RuleSource::UserSettings => McpScope::User,
            _ => McpScope::Project,
        };
        let (servers, found) = parse_node_servers(table, scope);
        warnings.extend(found.into_iter().map(|w| format!("{}: {w}", layer.path)));
        for found in servers {
            origins.insert(
                found.server.name.clone(),
                McpOrigin {
                    path: layer.path.to_owned(),
                    user: layer.source == RuleSource::UserSettings,
                },
            );
            merged.insert(found.server.name.clone(), found);
        }
    }
    // 停用的服务器在覆盖之后才分出来：用户级停用的服务器也遮住项目级的同名服务器。
    let (servers, disabled): (Vec<_>, Vec<_>) =
        merged.into_values().partition(|found| found.enabled);
    McpLayers {
        servers: servers.into_iter().map(|found| found.server).collect(),
        disabled: disabled.into_iter().map(|found| found.server).collect(),
        origins,
    }
}

#[cfg(test)]
#[path = "merge_tests.rs"]
mod tests;
