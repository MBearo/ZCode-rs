//! 插件提供的 hooks（docs/specs/rust-cli/hooks.md §2.2 第 5–7 条）。
//!
//! 插件的 `hooks/hooks.json` 与 manifest `hooks` 由 ext 读好（文件形状 `{"hooks": {…}}`），runtime 在
//! 插件加载后经 [`crate::HookEngine::register_plugins`] 交进来。与设置快照分开登记：入口层换设置
//! 快照时不会冲掉插件 hooks。

use serde_json::Value;

use crate::config::{HookCommand, HooksSettings};

/// 一个插件的 hooks。
#[derive(Debug, Clone, PartialEq)]
pub struct PluginHooks {
    pub plugin: String,
    /// 插件目录（`${CLAUDE_PLUGIN_ROOT}`）。
    pub root: String,
    /// 全部文件按顺序合并；command hook 都带着插件目录。
    pub settings: HooksSettings,
}

impl PluginHooks {
    /// 合并一个插件的 hooks 文件：同一事件的 matcher 按文件顺序拼接，
    /// `hooks` 为空的 matcher 跳过。返回值与警告。
    pub fn from_json(plugin: &str, root: &str, files: &[Value]) -> (Self, Vec<String>) {
        let mut settings = HooksSettings::default();
        let mut warnings = Vec::new();
        for file in files {
            let (parsed, more) = HooksSettings::from_json(file);
            warnings.extend(
                more.into_iter()
                    .map(|warning| format!("{plugin}: {warning}")),
            );
            for (event, matchers) in parsed.hooks {
                let mut matchers: Vec<_> = matchers
                    .into_iter()
                    .filter(|matcher| !matcher.hooks.is_empty())
                    .collect();
                for hook in matchers.iter_mut().flat_map(|matcher| &mut matcher.hooks) {
                    if let HookCommand::Command(command) = hook {
                        command.plugin_root = Some(root.to_owned());
                    }
                }
                match settings.hooks.iter_mut().find(|(known, _)| *known == event) {
                    Some((_, known)) => known.extend(matchers),
                    None if !matchers.is_empty() => settings.hooks.push((event, matchers)),
                    None => {}
                }
            }
        }
        (
            Self {
                plugin: plugin.to_owned(),
                root: root.to_owned(),
                settings,
            },
            warnings,
        )
    }
}

#[cfg(test)]
#[path = "plugin_tests.rs"]
mod tests;
