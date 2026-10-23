//! 实际生效的 hooks（docs/specs/rust-cli/hooks.md §2.2）：设置快照按来源合并，再接上插件的 hooks
//! （registered hooks）。

use std::collections::HashMap;

use rust_cli_kernel_api::hooks::HookEventName;

use crate::config::{HookMatcher, HookSource, HooksConfig, HooksSettings};
use crate::plugin::PluginHooks;

/// 合并后实际生效的配置。
#[derive(Debug, Clone, Default)]
pub(crate) struct Effective {
    pub matchers: HashMap<HookEventName, Vec<HookMatcher>>,
    pub allowed_urls: Option<Vec<String>>,
    pub allowed_env_vars: Option<Vec<String>>,
}

fn concat(values: impl Iterator<Item = Option<Vec<String>>>) -> Option<Vec<String>> {
    values.flatten().reduce(|mut all, next| {
        all.extend(next);
        all
    })
}

impl HooksConfig {
    /// 只有设置快照时的生效配置。
    #[cfg(test)]
    pub(crate) fn effective(&self) -> Effective {
        self.effective_with(&[])
    }

    /// 设置快照按来源的合并顺序合并，之后按插件顺序接上插件的 matcher；只用 managed hooks 时跳过插件。
    pub(crate) fn effective_with(&self, plugins: &[PluginHooks]) -> Effective {
        // 按来源的合并顺序（同一来源保持给出的顺序）。
        let mut sources: Vec<&(HookSource, HooksSettings)> = self.sources.iter().collect();
        sources.sort_by_key(|(source, _)| *source);
        let mut effective = Effective {
            allowed_urls: concat(
                sources
                    .iter()
                    .map(|(_, s)| s.allowed_http_hook_urls.clone()),
            ),
            allowed_env_vars: concat(
                sources
                    .iter()
                    .map(|(_, s)| s.http_hook_allowed_env_vars.clone()),
            ),
            ..Effective::default()
        };
        let policy = sources
            .iter()
            .filter(|(source, _)| *source == HookSource::Policy)
            .map(|(_, settings)| settings);
        if policy.clone().any(|s| s.disable_all_hooks == Some(true)) {
            return effective;
        }
        // 合并后的 disableAllHooks：后面的来源覆盖前面的。
        let merged_disable =
            sources.iter().rev().find_map(|(_, s)| s.disable_all_hooks) == Some(true);
        let managed_only = policy
            .clone()
            .any(|s| s.allow_managed_hooks_only == Some(true))
            || merged_disable;
        let allowed = |source: HookSource| !managed_only || source == HookSource::Policy;
        let plugins: &[PluginHooks] = if managed_only { &[] } else { plugins };
        for event in HookEventName::ALL {
            let matchers: Vec<HookMatcher> = sources
                .iter()
                .filter(|(source, _)| allowed(*source))
                .map(|(_, settings)| settings)
                .chain(plugins.iter().map(|plugin| &plugin.settings))
                .flat_map(|settings| settings.matchers(*event).cloned())
                .collect();
            if !matchers.is_empty() {
                effective.matchers.insert(*event, matchers);
            }
        }
        effective
    }
}
