//! `plugins` 段、插件存储根与插件候选（docs/specs/rust-cli/config.md §2.1、ext.md §4.1）。
//!
//! 合并按 Node `config-merger.ts`：`enabled` 按键覆盖，`dirs` 拼接去重，`enabledPlugins` 按 id 覆盖，
//! `suppressedBuiltins` 由最后写它的层整体替换；`extraKnownMarketplaces`、`options` 不读。候选目录在同一次
//! 组装里从本机存储的插件清单得出（platform `plugin_inventory`）。

use std::collections::BTreeMap;
use std::path::Path;

use rust_cli_ext::plugins::{PluginCandidate, PluginOrigin, PluginSources};
use rust_cli_kernel_api::env::EnvPath;
use rust_cli_paths::StoragePath;
use rust_cli_platform::plugin_inventory::{
    installed_plugins, installed_root, official_plugin_roots,
};
use serde_json::Value;

use super::merge::Layer;

/// Node `ZCODE_INLINE_PLUGIN_MARKETPLACE` 与 `ZCODE_OFFICIAL_PLUGIN_MARKETPLACE`。
const INLINE: &str = "inline";
const OFFICIAL: &str = rust_cli_base::names::OFFICIAL_MARKETPLACE;

/// 合并后的 `plugins` 段。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginSection {
    pub(crate) enabled: bool,
    pub(crate) dirs: Vec<String>,
    pub(crate) enabled_plugins: BTreeMap<String, bool>,
    pub(crate) suppressed_builtins: Vec<String>,
}

/// 非空字符串数组里的合格项；坏项记警告（D1）。
fn strings(
    path: &str,
    key: &str,
    value: &Value,
    warnings: &mut Vec<String>,
) -> Option<Vec<String>> {
    let Some(items) = value.as_array() else {
        warnings.push(format!("{path}: plugins.{key} must be an array of strings"));
        return None;
    };
    let mut found = Vec::new();
    for item in items {
        match item.as_str().filter(|text| !text.is_empty()) {
            Some(text) => found.push(text.to_owned()),
            None => warnings.push(format!("{path}: plugins.{key} skips {item}")),
        }
    }
    Some(found)
}

pub(crate) fn merge(layers: &[Layer<'_>], warnings: &mut Vec<String>) -> PluginSection {
    let mut merged = PluginSection {
        enabled: true,
        dirs: Vec::new(),
        enabled_plugins: BTreeMap::new(),
        suppressed_builtins: Vec::new(),
    };
    for layer in layers {
        let Some(section) = layer.object.get("plugins") else {
            continue;
        };
        let Some(section) = section.as_object() else {
            warnings.push(format!("{}: plugins must be an object", layer.path));
            continue;
        };
        if let Some(value) = section.get("enabled") {
            match value.as_bool() {
                Some(enabled) => merged.enabled = enabled,
                None => warnings.push(format!("{}: plugins.enabled must be a boolean", layer.path)),
            }
        }
        if let Some(dirs) = section
            .get("dirs")
            .and_then(|value| strings(layer.path, "dirs", value, warnings))
        {
            for dir in dirs {
                if !merged.dirs.contains(&dir) {
                    merged.dirs.push(dir);
                }
            }
        }
        match section.get("enabledPlugins").map(Value::as_object) {
            Some(Some(table)) => {
                for (id, on) in table {
                    match on.as_bool() {
                        Some(on) => {
                            merged.enabled_plugins.insert(id.clone(), on);
                        }
                        None => warnings.push(format!(
                            "{}: plugins.enabledPlugins.{id} must be a boolean",
                            layer.path
                        )),
                    }
                }
            }
            Some(None) => warnings.push(format!(
                "{}: plugins.enabledPlugins must be an object",
                layer.path
            )),
            None => {}
        }
        if let Some(suppressed) = section
            .get("suppressedBuiltins")
            .and_then(|value| strings(layer.path, "suppressedBuiltins", value, warnings))
        {
            merged.suppressed_builtins = suppressed;
        }
    }
    merged
}

/// 配置目录的位置（Node `resolve(rootPath)`）：绝对路径原样，相对路径相对工作区根；`.` 与 `..` 按字面
/// 化简，不展开 `~`。
///
/// 化简用 base 的 `path::normalize`（F58：以前这里自己写了一份）。
pub(crate) fn resolve_dir(root: &str, dir: &str) -> String {
    rust_cli_base::path::normalize(&Path::new(root).join(dir))
        .to_string_lossy()
        .into_owned()
}

/// 插件候选（ext.md §4.1 的来源 2–4，按 Node `resolveCandidates` 的顺序）。本机的插件目录与执行环境
/// 是同一个文件系统，路径直接用作 `EnvPath`。
pub(crate) async fn sources(
    section: PluginSection,
    root: &str,
    storage: &StoragePath,
) -> PluginSources {
    let mut candidates = Vec::new();
    if section.enabled {
        let candidate = |dir: String, marketplace: &str, origin| PluginCandidate {
            dir: EnvPath::new(dir),
            marketplace: marketplace.to_owned(),
            origin,
        };
        for dir in &section.dirs {
            candidates.push(candidate(
                resolve_dir(root, dir),
                INLINE,
                PluginOrigin::ConfigDir,
            ));
        }
        for dir in official_plugin_roots(storage).await {
            let dir = dir.to_string_lossy().into_owned();
            candidates.push(candidate(dir, OFFICIAL, PluginOrigin::Official));
        }
        for plugin in installed_plugins(storage).await {
            let dir = installed_root(storage, &plugin)
                .to_string_lossy()
                .into_owned();
            candidates.push(candidate(dir, &plugin.marketplace, PluginOrigin::Installed));
        }
    }
    PluginSources {
        candidates,
        enabled_plugins: section.enabled_plugins,
        suppressed_builtins: section.suppressed_builtins,
    }
}

#[cfg(test)]
#[path = "plugins_tests.rs"]
mod tests;
