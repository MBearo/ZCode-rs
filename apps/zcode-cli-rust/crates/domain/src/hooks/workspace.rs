//! Project hooks as a reviewable bundle (Node `shared/workspace-hook-config.ts`
//! and `workspace-hook-digest.ts`): entries, declaration and bundle digests,
//! and their registrations once admitted.
use super::digest::{Slot, declaration_digest, optional, sha256};
use super::{HookEvent, Program, Registration, Shell, SourceKind, program, timeout_ms};
use serde_json::{Value, json};
use std::path::Path;

/// A project config file that declares hooks (Node `WorkspaceHookSourceInput`).
#[derive(Clone, Debug, PartialEq)]
pub struct Source {
    pub canonical_path: String,
    pub base_dir: String,
    /// Position among every discovered project config file.
    pub discovery_order: usize,
    /// `zcode.json`, `.zcode/config.json` or `explicit`.
    pub kind: &'static str,
    pub explicit: bool,
    /// Only `<cwd>/.zcode/config.json` can be toggled from a review.
    pub editable: bool,
    pub hooks: Value,
}

/// Node `createWorkspaceHookSourceInput` (paths already absolute).
pub fn source(path: &str, working_directory: &str, hooks: Value, order: usize) -> Source {
    let file = Path::new(path);
    let directory = file.parent().unwrap_or(Path::new(""));
    let base = if directory.file_name().is_some_and(|n| n == ".zcode") {
        directory.parent().unwrap_or(directory)
    } else {
        directory
    };
    let editable_path = Path::new(working_directory)
        .join(".zcode")
        .join("config.json");
    Source {
        canonical_path: path.into(),
        base_dir: base.to_string_lossy().into_owned(),
        discovery_order: order,
        kind: if file.file_name().is_some_and(|n| n == "zcode.json") {
            "zcode.json"
        } else {
            ".zcode/config.json"
        },
        explicit: false,
        editable: file == editable_path,
        hooks,
    }
}

/// Node `WorkspaceHookRuntimeRoot`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RuntimeRoot {
    pub enabled: bool,
    pub timeout_ms: u64,
    pub max_output_bytes: u64,
}

/// Node `resolveWorkspaceHookRuntimeRoot` over default, user, project, env and
/// CLI `hooks` roots (later defined values win).
pub fn runtime_root(roots: &[&Value]) -> RuntimeRoot {
    let mut enabled = false;
    let mut timeout = 60_000.0;
    let mut max_output = 32_768.0;
    for root in roots {
        enabled |= root["enabled"] == true;
        timeout = root["timeoutMs"].as_f64().unwrap_or(timeout);
        max_output = root["maxOutputBytes"].as_f64().unwrap_or(max_output);
    }
    let round = |v: f64| (v + 0.5).floor().max(1.0) as u64;
    RuntimeRoot {
        enabled,
        timeout_ms: round(timeout),
        max_output_bytes: round(max_output),
    }
}

/// One project hook (Node `CanonicalWorkspaceHookEntry`, strings trimmed like
/// the contracts schema after the digest was taken on the raw values).
#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub review_item_id: String,
    pub event: HookEvent,
    pub matcher_index: usize,
    pub hook_index: usize,
    pub source_file_index: usize,
    pub source_relative_path: String,
    pub matcher: Option<String>,
    pub program: Program,
    pub resolved_timeout_ms: u64,
    pub resolved_max_output_bytes: u64,
    pub status_message: Option<String>,
    pub source_root_enabled: bool,
    pub declaration_enabled: bool,
    pub runtime_hooks_enabled: bool,
    pub configured_enabled: bool,
    pub editable: bool,
    pub digest: String,
}

impl Entry {
    /// Node `displayCommandAtGrant` / review `displayCommand`.
    pub fn display_command(&self) -> String {
        match &self.program {
            Program::Command { command, .. } => command.clone(),
            Program::Process { command, args } => std::iter::once(command)
                .chain(args)
                .cloned()
                .collect::<Vec<_>>()
                .join(" "),
        }
    }
    pub fn kind(&self) -> &'static str {
        match self.program {
            Program::Command { .. } => "command",
            Program::Process { .. } => "process",
        }
    }
}

/// Node `WorkspaceHookBundleSnapshot`.
#[derive(Clone, Debug, PartialEq)]
pub struct Snapshot {
    pub workspace_identity: String,
    pub source_files: Vec<Source>,
    pub hooks: Vec<Entry>,
    pub bundle_digest: String,
}

/// Node `normalizeRelativeSourcePath`.
fn relative_path(workspace: &str, path: &str) -> String {
    let relative = relative(Path::new(workspace), Path::new(path)).replace('\\', "/");
    if relative.is_empty() {
        Path::new(path)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    } else {
        relative
    }
}

/// Node `path.relative` on normalized absolute paths.
fn relative(from: &Path, to: &Path) -> String {
    let from: Vec<_> = from.components().collect();
    let to: Vec<_> = to.components().collect();
    let common = from.iter().zip(&to).take_while(|(a, b)| a == b).count();
    let mut parts: Vec<String> = vec!["..".into(); from.len() - common];
    parts.extend(
        to[common..]
            .iter()
            .map(|c| c.as_os_str().to_string_lossy().into_owned()),
    );
    parts.join(std::path::MAIN_SEPARATOR_STR)
}

fn trimmed(value: &str) -> String {
    value.trim().to_owned()
}

/// Node `buildWorkspaceHookBundleSnapshot`: `None` when no source declares a hook.
pub fn snapshot(
    identity: &str,
    workspace_path: &str,
    sources: &[Source],
    root: RuntimeRoot,
) -> Option<Snapshot> {
    let mut hooks = vec![];
    let mut bundle_hooks = vec![];
    for (source_index, source) in sources.iter().enumerate() {
        let relative = relative_path(workspace_path, &source.canonical_path);
        let source_enabled = source.hooks["enabled"] != false;
        for event in HookEvent::ALL {
            let groups = source.hooks["events"][event.as_str()].as_array();
            for (matcher_index, group) in groups.into_iter().flatten().enumerate() {
                let matcher = group.get("matcher").cloned().unwrap_or(Value::Null);
                for (hook_index, raw) in group["hooks"].as_array().into_iter().flatten().enumerate()
                {
                    let Some(program) = program(raw) else {
                        continue;
                    };
                    let program = match program {
                        Program::Command {
                            command,
                            shell,
                            background,
                        } => Program::Command {
                            command: trimmed(&command),
                            shell: match shell {
                                Shell::Path(path) => Shell::Path(trimmed(&path)),
                                other => other,
                            },
                            background,
                        },
                        Program::Process { command, args } => Program::Process {
                            command: trimmed(&command),
                            args,
                        },
                    };
                    let declaration_enabled = raw["enabled"] != false;
                    let mut entry = Entry {
                        review_item_id: format!(
                            "workspace-hook-{source_index}-{}-{matcher_index}-{hook_index}",
                            event.as_str()
                        ),
                        event,
                        matcher_index,
                        hook_index,
                        source_file_index: source_index,
                        source_relative_path: relative.clone(),
                        matcher: matcher.as_str().map(str::to_owned),
                        program,
                        resolved_timeout_ms: timeout_ms(raw, root.timeout_ms as f64),
                        resolved_max_output_bytes: root.max_output_bytes,
                        status_message: raw["statusMessage"].as_str().map(trimmed),
                        source_root_enabled: source_enabled,
                        declaration_enabled,
                        runtime_hooks_enabled: root.enabled,
                        configured_enabled: source_enabled && declaration_enabled && root.enabled,
                        editable: source.editable,
                        digest: String::new(),
                    };
                    entry.digest = declaration_digest(
                        raw,
                        &Slot {
                            relative_path: &relative,
                            discovery_order: source.discovery_order,
                            event,
                            matcher: &matcher,
                            matcher_index,
                            hook_index,
                            default_timeout_ms: root.timeout_ms,
                            max_output_bytes: root.max_output_bytes,
                        },
                    );
                    bundle_hooks.push(json!([
                        entry.digest,
                        entry.source_root_enabled,
                        entry.declaration_enabled,
                        entry.runtime_hooks_enabled,
                        entry.configured_enabled
                    ]));
                    hooks.push(entry);
                }
            }
        }
    }
    if hooks.is_empty() {
        return None;
    }
    let bundle_sources: Vec<Value> = sources
        .iter()
        .map(|s| {
            json!([
                relative_path(workspace_path, &s.canonical_path),
                s.discovery_order,
                s.kind,
                s.explicit,
                optional(&s.hooks["enabled"]),
                optional(&s.hooks["timeoutMs"]),
                optional(&s.hooks["maxOutputBytes"])
            ])
        })
        .collect();
    let bundle_digest = sha256(&json!([
        "workspace-hook-bundle",
        1,
        bundle_sources,
        bundle_hooks
    ]));
    Some(Snapshot {
        workspace_identity: trimmed(identity),
        source_files: sources
            .iter()
            .map(|s| Source {
                canonical_path: trimmed(&s.canonical_path),
                base_dir: trimmed(&s.base_dir),
                ..s.clone()
            })
            .collect(),
        hooks,
        bundle_digest,
    })
}

/// Node `config-factory` snapshot: candidates of the config snapshot, runtime
/// root over default, user and project `hooks` roots (Rust has no env or CLI
/// hooks).
pub fn discover(
    config: &crate::config::ConfigSnapshot,
    identity: &str,
    workspace: &str,
) -> Option<Snapshot> {
    let sources: Vec<Source> = config
        .project_hook_candidates
        .iter()
        .map(|c| source(&c.path, workspace, c.hooks.clone(), c.discovery_order))
        .collect();
    let default = json!({"enabled":false,"timeoutMs":60000,"maxOutputBytes":32768});
    let mut roots = vec![&default, &config.user_hooks];
    roots.extend(config.project_hook_candidates.iter().map(|c| &c.hooks));
    snapshot(identity, workspace, &sources, runtime_root(&roots))
}

/// Node `createWorkspaceHookRegistrations` for one snapshot.
pub fn registrations(snapshot: &Snapshot) -> Vec<Registration> {
    snapshot
        .hooks
        .iter()
        .map(|entry| Registration {
            event: entry.event,
            matcher: entry.matcher.clone(),
            program: entry.program.clone(),
            source: format!("project.{}", entry.review_item_id),
            source_kind: SourceKind::Project,
            source_path: snapshot
                .source_files
                .get(entry.source_file_index)
                .map(|s| s.canonical_path.clone()),
            plugin: None,
            status_message: entry.status_message.clone(),
            timeout_ms: entry.resolved_timeout_ms,
            max_output_bytes: entry.resolved_max_output_bytes as usize,
            review: Some((entry.review_item_id.clone(), entry.digest.clone())),
        })
        .collect()
}

/// Node `insertWorkspaceHooks`: per event, project hooks go before the first
/// hook that is not a user hook (after user hooks, before plugins).
pub fn insert(configured: &[Registration], workspace: Vec<Registration>) -> Vec<Registration> {
    let mut result = configured.to_vec();
    let mut events: Vec<HookEvent> = vec![];
    for r in &workspace {
        if !events.contains(&r.event) {
            events.push(r.event);
        }
    }
    for event in events {
        let batch: Vec<Registration> = workspace
            .iter()
            .filter(|r| r.event == event)
            .cloned()
            .collect();
        let at = result
            .iter()
            .position(|r| r.event == event && r.source_kind != SourceKind::User)
            .unwrap_or(result.len());
        result.splice(at..at, batch);
    }
    result
}
