//! `/hooks` 的只读快照（docs/specs/rust-cli/tui-display.md §3.5，工作线 L28）：设置快照与插件的 hooks，按来源的
//! 合并顺序（User → Project → Local → Policy → 插件）、来源内按事件与 matcher 的书写顺序列出。
use rust_cli_kernel_api::manage::HookEntry;

use crate::config::{HookCommand, HookSource, HooksSettings};
use crate::{HookEngine, PluginHooks};

fn source_name(source: HookSource) -> &'static str {
    match source {
        HookSource::User => "user",
        HookSource::Project => "project",
        HookSource::Local => "local",
        HookSource::Policy => "policy",
    }
}

fn kind(hook: &HookCommand) -> &'static str {
    match hook {
        HookCommand::Command(_) => "command",
        HookCommand::Prompt(_) => "prompt",
        HookCommand::Agent(_) => "agent",
        HookCommand::Http(_) => "http",
    }
}

fn entries_of(settings: &HooksSettings, source: &str, out: &mut Vec<HookEntry>) {
    for (event, matchers) in &settings.hooks {
        for matcher in matchers {
            for hook in &matcher.hooks {
                out.push(HookEntry {
                    event: event.as_str().to_owned(),
                    matcher: matcher.matcher.clone(),
                    kind: kind(hook).to_owned(),
                    display: hook.display(),
                    source: source.to_owned(),
                });
            }
        }
    }
}

/// 配置里写了的全部 hook（不按 `disableAllHooks` 与信任过滤：查看的是配置本身）。
pub(crate) fn entries(
    sources: &[(HookSource, HooksSettings)],
    plugins: &[PluginHooks],
) -> Vec<HookEntry> {
    let mut out = Vec::new();
    for (source, settings) in sources {
        entries_of(settings, source_name(*source), &mut out);
    }
    for plugin in plugins {
        entries_of(
            &plugin.settings,
            &format!("plugin:{}", plugin.plugin),
            &mut out,
        );
    }
    out
}

impl HookEngine {
    /// 当前快照里的 hooks（`Kernel::hooks`）。
    pub fn entries(&self) -> Vec<HookEntry> {
        let loaded = self.snapshot();
        entries(&loaded.config.sources, &loaded.plugins)
    }
}

#[cfg(test)]
#[path = "listing_tests.rs"]
mod tests;
