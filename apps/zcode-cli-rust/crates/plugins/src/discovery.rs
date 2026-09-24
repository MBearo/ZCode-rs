//! Plugin discovery (Node `NodePluginAdapter.discoverPluginsSync` with the
//! candidates of `resolveZCodePlugins`). Spec rust-m10-plugins §3.
use crate::components::Group;
use crate::loaded::{Loaded, Source};
use crate::manifest::{Diagnostic, Severity};
use anyhow::Result;
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use tokio_util::sync::CancellationToken;
use zcode_cli_domain::hooks::HookEvent;

pub struct Request<'a> {
    /// The effective configuration (`plugins` section and `storage.dir`).
    pub config: &'a Value,
    pub storage: &'a Path,
    pub cwd: &'a Path,
    pub env: &'a (dyn Fn(&str) -> Option<String> + Sync),
    pub cancel: &'a CancellationToken,
}

/// Node `PluginMetadata`.
pub struct Plugin {
    pub loaded: Loaded,
    pub enabled: bool,
    pub data_path: PathBuf,
    pub components: Vec<Group>,
    pub declared_mcp: Vec<String>,
    pub mcp_server_names: Vec<String>,
    pub hook_details: Vec<Value>,
    pub skill_count: usize,
    pub skill_root_count: usize,
    pub command_root_count: usize,
    pub configured_options: Map<String, Value>,
}

/// A skill root contributed by an enabled plugin.
pub struct SkillRoot {
    pub path: PathBuf,
    pub plugin_id: String,
    pub plugin_name: String,
    pub plugin_root: PathBuf,
}

#[derive(Default)]
pub struct Outcome {
    pub plugins: Vec<Plugin>,
    pub diagnostics: Vec<Diagnostic>,
    pub skill_roots: Vec<SkillRoot>,
    /// Validated matchers of enabled plugins, each hook carrying its plugin context.
    pub hooks: Vec<(HookEvent, Value)>,
    /// `plugin:<name>:<key>` → server config; a later plugin replaces a name.
    pub mcp_servers: Vec<(String, Value)>,
}

struct Candidate {
    root: PathBuf,
    marketplace: String,
    source: Source,
    default_enabled: bool,
}

async fn candidates(
    request: &Request<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Result<Vec<Candidate>> {
    let mut list: Vec<Candidate> = crate::fsx::path_list(&request.config["plugins"]["dirs"])
        .iter()
        .map(|dir| Candidate {
            root: crate::fsx::resolve(request.cwd, dir),
            marketplace: crate::official::INLINE.into(),
            source: Source::Inline,
            default_enabled: true,
        })
        .collect();
    for root in crate::records::official_roots(request.storage, diagnostics).await {
        list.push(Candidate {
            root,
            marketplace: crate::official::MARKETPLACE.into(),
            source: Source::Official,
            default_enabled: false,
        });
    }
    for record in crate::records::installed(request.storage).await? {
        list.push(Candidate {
            root: crate::records::installed_root(request.storage, &record).await?,
            marketplace: record.marketplace,
            source: Source::Cache,
            default_enabled: false,
        });
    }
    Ok(list)
}

/// Node `loadPlugin`.
async fn load(candidate: &Candidate, diagnostics: &mut Vec<Diagnostic>) -> Option<Loaded> {
    if !crate::fsx::is_dir(&candidate.root).await {
        let message = format!("Plugin root does not exist: {}", candidate.root.display());
        diagnostics.push(
            Diagnostic::new("plugin_root_not_found", Severity::Warning, message)
                .at(&candidate.root),
        );
        return None;
    }
    let Some(manifest_path) = crate::manifest::find(&candidate.root).await else {
        let message = format!("Plugin manifest not found: {}", candidate.root.display());
        diagnostics.push(
            Diagnostic::new("plugin_manifest_not_found", Severity::Error, message)
                .at(&candidate.root),
        );
        return None;
    };
    match crate::manifest::read(&manifest_path).await {
        Ok(manifest) => Some(Loaded {
            id: format!(
                "{}@{}",
                manifest["name"].as_str().unwrap_or_default(),
                candidate.marketplace
            ),
            manifest,
            manifest_path,
            marketplace: candidate.marketplace.clone(),
            root: candidate.root.clone(),
            source: candidate.source,
        }),
        Err(message) => {
            diagnostics.push(
                Diagnostic::new("plugin_manifest_invalid", Severity::Error, message)
                    .at(&manifest_path),
            );
            None
        }
    }
}

/// Node `warnEmptyDeclaredSkillRoots`.
async fn warn_empty_skill_roots(loaded: &Loaded, diagnostics: &mut Vec<Diagnostic>) {
    let mut seen = vec![];
    for raw in crate::fsx::path_list(&loaded.manifest["skills"]) {
        let Some(path) = crate::fsx::resolve_inside(&loaded.root, &raw) else {
            continue;
        };
        if seen.contains(&path) {
            continue;
        }
        seen.push(path.clone());
        let message = if crate::fsx::is_missing(&path).await {
            format!("Plugin skills path does not exist: {raw}")
        } else {
            match crate::components::scan_skills(&path, false).await {
                Ok(files) if files.is_empty() => {
                    format!("Plugin skills path does not contain any skills: {raw}")
                }
                _ => continue,
            }
        };
        diagnostics.push(
            Diagnostic::new("plugin_skill_root_empty", Severity::Warning, message)
                .at(path)
                .plugin(&loaded.id),
        );
    }
}

/// Node `countSkillFiles`: distinct `SKILL.md` files; unreadable roots count 0.
async fn count_skills(roots: &[PathBuf]) -> usize {
    let mut files = std::collections::BTreeSet::new();
    for root in roots {
        if let Ok(found) = crate::components::scan_skills(root, false).await {
            files.extend(found);
        }
    }
    files.len()
}

fn unsupported_components(loaded: &Loaded, diagnostics: &mut Vec<Diagnostic>) {
    for key in ["channels", "lspServers", "outputStyles", "settings"] {
        if loaded.manifest.get(key).is_some() {
            diagnostics.push(
                Diagnostic::new(
                    "plugin_unsupported_component",
                    Severity::Warning,
                    format!("Plugin component is diagnostic-only in this ZCode runtime: {key}"),
                )
                .at(&loaded.manifest_path)
                .plugin(&loaded.id),
            );
        }
    }
}

/// Node `discoverPluginsSync`.
pub async fn discover(request: &Request<'_>) -> Result<Outcome> {
    let plugins_config = &request.config["plugins"];
    let mut outcome = Outcome::default();
    if plugins_config["enabled"] == false {
        return Ok(outcome);
    }
    let mut diagnostics = std::mem::take(&mut outcome.diagnostics);
    let diagnostics = &mut diagnostics;
    let suppressed = crate::fsx::path_list(&plugins_config["suppressedBuiltins"]);
    let data_root = request.storage.join("data");
    let mut seen = vec![];
    for candidate in candidates(request, diagnostics).await? {
        anyhow::ensure!(!request.cancel.is_cancelled(), "Plugin operation cancelled");
        let Some(loaded) = load(&candidate, diagnostics).await else {
            continue;
        };
        if loaded.source == Source::Official && suppressed.contains(&loaded.id) {
            continue;
        }
        if seen.contains(&loaded.id) {
            diagnostics.push(
                Diagnostic::new(
                    "plugin_duplicate_id",
                    Severity::Warning,
                    format!("Duplicate plugin ignored: {}", loaded.id),
                )
                .at(&loaded.root)
                .plugin(&loaded.id),
            );
            continue;
        }
        seen.push(loaded.id.clone());
        unsupported_components(&loaded, diagnostics);
        let default = candidate.default_enabled || crate::official::enabled_by_default(&loaded.id);
        let enabled = plugins_config["enabledPlugins"][&loaded.id]
            .as_bool()
            .unwrap_or(default);
        let data_path = data_root.join(crate::fsx::sanitize_id(&loaded.id));
        let definitions = crate::mcp::definitions(&loaded, diagnostics).await;
        let sources = crate::hooks::sources(&loaded, diagnostics).await;
        let inspection = crate::hooks::inspect(&sources, &loaded, &data_path, diagnostics);
        let options = plugins_config["options"][&loaded.id]
            .as_object()
            .cloned()
            .unwrap_or_default();
        let (mut skill_roots, mut command_root_count, mut servers) = (vec![], 0, vec![]);
        if enabled {
            tokio::fs::create_dir_all(&data_path).await?;
            warn_empty_skill_roots(&loaded, diagnostics).await;
            skill_roots = crate::commands::roots("skills", &loaded, diagnostics).await;
            command_root_count = crate::commands::roots("commands", &loaded, diagnostics)
                .await
                .len();
            if crate::commands::materialize(&loaded, &data_path, diagnostics).await {
                command_root_count += 1;
            }
            let context = crate::mcp::Context {
                loaded: &loaded,
                data_path: &data_path,
                cwd: request.cwd,
                env: request.env,
                options: &options,
            };
            servers = crate::mcp::resolve(&definitions, &context, diagnostics);
            outcome.hooks.extend(inspection.events);
        }
        let skill_count = if enabled {
            count_skills(&skill_roots).await
        } else {
            0
        };
        for (name, config) in &servers {
            outcome.mcp_servers.retain(|(n, _)| n != name);
            outcome.mcp_servers.push((name.clone(), config.clone()));
        }
        outcome
            .skill_roots
            .extend(skill_roots.iter().map(|path| SkillRoot {
                path: path.clone(),
                plugin_id: loaded.id.clone(),
                plugin_name: loaded.name().to_owned(),
                plugin_root: loaded.root.clone(),
            }));
        let components =
            crate::components::enumerate(&loaded.root, Some(&loaded.manifest), Some(&loaded)).await;
        outcome.plugins.push(Plugin {
            components,
            declared_mcp: definitions.into_iter().map(|(k, _)| k).collect(),
            mcp_server_names: servers.into_iter().map(|(n, _)| n).collect(),
            hook_details: inspection.details,
            skill_count,
            skill_root_count: skill_roots.len(),
            command_root_count,
            configured_options: options,
            data_path,
            enabled,
            loaded,
        });
    }
    outcome.diagnostics = std::mem::take(diagnostics);
    Ok(outcome)
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
