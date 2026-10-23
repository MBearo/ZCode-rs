//! 项目级 hooks 的信任（docs/specs/rust-cli/config.md §4）：digest 与 Node 逐字节一致，信任记录与 Node
//! CLI、App 共用。只有已信任且 `configuredEnabled` 的声明进 hooks 快照；其余是 pending。

use std::collections::HashSet;

use rust_cli_hooks::config::{HookMatcher, HooksSettings};
use rust_cli_hooks::node_config::{NODE_EVENTS, NodeHooks};
use rust_cli_hooks::workspace_digest::{Bundle, DigestEntry, DigestSource, RuntimeRoot};
use rust_cli_hooks::{HookSource, workspace_digest};
use rust_cli_paths::StoragePaths;
use rust_cli_platform::{HookTrustStore, TrustLoad};
use serde_json::{Map, Value};

use super::files::{ProjectFile, relative};

/// 含 `hooks` 的项目文件与解析出的 hooks；Node 7 个事件以外的事件去掉并记警告（D2）。
pub(crate) fn candidates<'a>(
    files: &'a [ProjectFile],
    warnings: &mut Vec<String>,
) -> Vec<(&'a ProjectFile, NodeHooks)> {
    let mut found = Vec::new();
    for file in files {
        let Some(value) = file.file.object().and_then(|object| object.get("hooks")) else {
            continue;
        };
        let (mut hooks, parsed) = NodeHooks::from_json(value);
        warnings.extend(parsed.into_iter().map(|w| format!("{}: {w}", file.path)));
        hooks.events.retain(|(event, _)| {
            let kept = NODE_EVENTS.contains(event);
            if !kept {
                warnings.push(format!(
                    "{}: project hooks for {} are not supported (only the Node CLI events can be trusted)",
                    file.path,
                    event.as_str()
                ));
            }
            kept
        });
        found.push((file, hooks));
    }
    found
}

/// digest 的来源（Node `WorkspaceHookSourceInput`）。
pub(crate) fn sources<'a>(
    root: &str,
    candidates: &'a [(&ProjectFile, NodeHooks)],
) -> Vec<DigestSource<'a>> {
    candidates
        .iter()
        .map(|(file, hooks)| DigestSource {
            relative_path: relative(root, &file.path),
            discovery_order: file.order,
            kind: file.kind.as_str(),
            explicit: false,
            editable: file.editable,
            hooks,
        })
        .collect()
}

/// 一个工作区的项目 hooks：digest、信任记录与准入（启动组装与 `zcode hooks trust` 共用）。
pub(crate) struct WorkspaceHooks {
    /// 工作区身份：工作区根的绝对路径（Node 独立 CLI 的 `resolve(cwd)`）。
    pub(crate) identity: String,
    pub(crate) root: RuntimeRoot,
    pub(crate) bundle: Option<Bundle>,
    pub(crate) store: HookTrustStore,
    pub(crate) load: TrustLoad,
}

/// 用户级配置的 `storage.dir`（信任记录的存储根，config.md §4.2）。
fn storage_dir(user: Option<&Map<String, Value>>) -> Option<&str> {
    user?.get("storage")?.get("dir")?.as_str()
}

impl WorkspaceHooks {
    pub(crate) async fn inspect(
        workspace_root: &str,
        user: Option<&Map<String, Value>>,
        files: &[ProjectFile],
        paths: &StoragePaths,
        warnings: &mut Vec<String>,
    ) -> Self {
        let user_hooks = user
            .and_then(|object| object.get("hooks"))
            .map(|value| NodeHooks::from_json(value).0)
            .unwrap_or_default();
        let found = candidates(files, warnings);
        let root = workspace_digest::runtime_root(
            std::iter::once(&user_hooks).chain(found.iter().map(|(_, hooks)| hooks)),
        );
        let bundle = workspace_digest::bundle(&sources(workspace_root, &found), &root);
        let store = HookTrustStore::new(paths.hook_trust_store(storage_dir(user)));
        let load = store.load().await;
        Self {
            identity: workspace_root.to_owned(),
            root,
            bundle,
            store,
            load,
        }
    }

    pub(crate) fn entries(&self) -> &[DigestEntry] {
        self.bundle.as_ref().map_or(&[], |bundle| &bundle.entries)
    }

    pub(crate) fn trusted(&self) -> HashSet<String> {
        self.load.trusted(&self.identity).into_iter().collect()
    }

    /// 已信任且启用的声明，按来源 → 事件 → matcher 分组成 hooks 快照的来源（`HookSource::Project`）。
    pub(crate) fn admitted(&self) -> Vec<(HookSource, HooksSettings)> {
        let trusted = self.trusted();
        let mut warnings = Vec::new();
        // 声明已按 来源 → 事件 → matcher → hook 排好（digest 的遍历顺序），相邻的合成一组。
        let mut groups: Vec<(&DigestEntry, HookMatcher)> = Vec::new();
        for entry in self.entries() {
            if !entry.configured_enabled || !trusted.contains(&entry.digest) {
                continue;
            }
            let command = entry.hook.to_command(self.root.timeout_ms, &mut warnings);
            match groups.last_mut() {
                Some((first, matcher))
                    if first.source_index == entry.source_index
                        && first.event == entry.event
                        && first.matcher_index == entry.matcher_index =>
                {
                    matcher.hooks.push(command);
                }
                _ => groups.push((
                    entry,
                    HookMatcher {
                        matcher: entry.matcher.clone(),
                        hooks: vec![command],
                    },
                )),
            }
        }
        for warning in warnings {
            tracing::warn!(warning, "project hooks");
        }
        let mut sources: Vec<(usize, HooksSettings)> = Vec::new();
        for (first, matcher) in groups {
            if sources.last().map(|(index, _)| *index) != Some(first.source_index) {
                sources.push((first.source_index, HooksSettings::default()));
            }
            if let Some((_, settings)) = sources.last_mut() {
                match settings.hooks.last_mut() {
                    Some((event, matchers)) if *event == first.event => matchers.push(matcher),
                    _ => settings.hooks.push((first.event, vec![matcher])),
                }
            }
        }
        sources
            .into_iter()
            .map(|(_, settings)| (HookSource::Project, settings))
            .collect()
    }

    /// 启用但没有信任记录的声明（Node `-p` 提示里的 pending digest）。
    pub(crate) fn pending(&self) -> Option<Pending> {
        let trusted = self.trusted();
        let digests: Vec<String> = self
            .entries()
            .iter()
            .filter(|entry| entry.configured_enabled && !trusted.contains(&entry.digest))
            .map(|entry| entry.digest.clone())
            .collect();
        let bundle = self.bundle.as_ref()?;
        (!digests.is_empty()).then(|| Pending {
            identity: self.identity.clone(),
            bundle_digest: bundle.bundle_digest.clone(),
            digests,
        })
    }
}

/// 未信任的项目 hooks（config.md §4.4）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Pending {
    pub(crate) identity: String,
    pub(crate) bundle_digest: String,
    pub(crate) digests: Vec<String>,
}

impl Pending {
    /// Node `writeHeadlessWorkspaceHookTrustDiagnostic` 写到 stderr 的文本（末尾有换行）。
    pub(crate) fn notice(&self) -> String {
        let mut lines = vec![
            "Workspace Hooks skipped: workspace_hooks_pending_trust".to_owned(),
            format!("workspace: {}", self.identity),
            format!("bundle: {}", self.bundle_digest),
        ];
        lines.extend(self.digests.iter().map(|d| format!("pending digest: {d}")));
        let quoted = serde_json::to_string(&self.identity).unwrap_or_default();
        lines.push(format!(
            "Review with: zcode hooks trust review --workspace {quoted}"
        ));
        format!("{}\n", lines.join("\n"))
    }
}

#[cfg(test)]
#[path = "trust_tests.rs"]
mod tests;
