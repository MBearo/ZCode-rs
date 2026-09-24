//! Component inventory of a plugin root (Node `plugin-components.ts`) and the
//! shared skill scan (Node `skills/scan.ts`; plugin scans never follow links).
use crate::loaded::Loaded;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

pub struct Group {
    pub kind: &'static str,
    pub items: Vec<(String, Option<String>)>,
}

impl Group {
    pub fn protocol(&self) -> Value {
        let items: Vec<Value> = self
            .items
            .iter()
            .map(|(name, description)| match description {
                Some(description) => json!({"name":name,"description":description}),
                None => json!({ "name": name }),
            })
            .collect();
        json!({"kind":self.kind,"items":items})
    }
}

/// Node `SKILL_SCAN_EXCLUDED_DIRECTORY_NAMES` and the dot-directory allowlist.
fn walkable_name(name: &str) -> bool {
    const EXCLUDED: [&str; 12] = [
        "node_modules",
        "dist",
        "build",
        "out",
        "target",
        "vendor",
        "coverage",
        ".cache",
        ".next",
        ".turbo",
        ".venv",
        "__pycache__",
    ];
    !EXCLUDED.contains(&name) && (!name.starts_with('.') || name == ".system")
}

fn expected_missing(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
    )
}

async fn loadable(path: &Path, follow: bool) -> std::io::Result<bool> {
    if !follow && crate::fsx::is_symlink(path).await {
        return Ok(false);
    }
    match tokio::fs::metadata(path).await {
        Ok(metadata) => Ok(metadata.is_file()),
        Err(error) if expected_missing(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Node `scanSkillFilesUnderRoot`: `SKILL.md` of the root and of each walkable
/// child directory. Missing roots are empty; other errors are returned.
pub async fn scan_skills(root: &Path, follow: bool) -> std::io::Result<Vec<PathBuf>> {
    if !follow && crate::fsx::is_symlink(root).await {
        return Ok(vec![]);
    }
    match tokio::fs::metadata(root).await {
        Ok(metadata) if metadata.is_dir() => {}
        Ok(_) => return Ok(vec![]),
        Err(error) if expected_missing(&error) => return Ok(vec![]),
        Err(error) => return Err(error),
    }
    let mut files = vec![];
    let own = root.join("SKILL.md");
    if loadable(&own, follow).await? {
        files.push(own);
    }
    for (name, kind) in crate::fsx::read_dir_names(root).await? {
        if !(kind.is_dir() || (follow && kind.is_symlink())) || !walkable_name(&name) {
            continue;
        }
        let candidate = root.join(&name).join("SKILL.md");
        if loadable(&candidate, follow).await? {
            files.push(candidate);
        }
    }
    Ok(files)
}

/// Node `collectComponentDirs`: the default directory when it exists, then
/// the manifest's path or paths, resolved inside the root and deduplicated.
pub async fn component_dirs(root: &Path, field: &Value, default: &str) -> Vec<PathBuf> {
    let mut raws = vec![];
    if crate::fsx::is_dir(&root.join(default)).await {
        raws.push(default.to_owned());
    }
    raws.extend(crate::fsx::path_list(field));
    let mut dirs: Vec<PathBuf> = vec![];
    for raw in raws {
        let raw = raw.strip_prefix("./").unwrap_or(&raw);
        if let Some(path) = crate::fsx::resolve_inside(root, raw)
            && !dirs.contains(&path)
        {
            dirs.push(path);
        }
    }
    dirs
}

type Items = Vec<(String, Option<String>)>;

async fn markdown(root: &Path, field: &Value, default: &str) -> Items {
    let mut items: Items = vec![];
    if let Value::Object(declared) = field {
        for (raw, meta) in declared {
            let name = crate::js::trim(raw);
            if name.is_empty() || items.iter().any(|(n, _)| n == name) {
                continue;
            }
            let description = meta["description"]
                .as_str()
                .map(|d| crate::js::trim(d).to_owned())
                .filter(|d| !d.is_empty());
            items.push((name.to_owned(), description));
        }
    }
    for dir in component_dirs(root, field, default).await {
        let Ok(entries) = crate::fsx::read_dir_names(&dir).await else {
            continue;
        };
        for (file, kind) in entries {
            let Some(base) = file.strip_suffix(".md").filter(|_| kind.is_file()) else {
                continue;
            };
            let front = crate::frontmatter::read(&dir.join(&file)).await;
            let name = front.name.unwrap_or_else(|| base.to_owned());
            if !items.iter().any(|(n, _)| *n == name) {
                items.push((name, front.description));
            }
        }
    }
    items
}

async fn skills(root: &Path, field: &Value) -> Items {
    let mut items: Items = vec![];
    let mut files = vec![];
    for dir in component_dirs(root, field, "skills").await {
        let Ok(found) = scan_skills(&dir, false).await else {
            continue;
        };
        for file in found {
            if files.contains(&file) {
                continue;
            }
            let fallback = file
                .parent()
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let front = crate::frontmatter::read(&file).await;
            files.push(file);
            let name = front.name.unwrap_or(fallback);
            if !items.iter().any(|(n, _)| *n == name) {
                items.push((name, front.description));
            }
        }
    }
    items
}

/// Names of the inline `hooks` object (with an optional `hooks` wrapper).
fn inline_hook_events(value: &Value) -> Vec<String> {
    let field = match &value["hooks"] {
        Value::Object(_) => &value["hooks"],
        _ => value,
    };
    let mut names: Vec<String> = vec![];
    for key in field.as_object().into_iter().flat_map(|o| o.keys()) {
        let name = crate::js::trim(key);
        if !name.is_empty() && !names.iter().any(|n| n == name) {
            names.push(name.to_owned());
        }
    }
    names
}

/// Node `enumeratePluginComponents`: agent, command, skill, hook and mcp
/// groups (empty groups omitted), independent of the enabled state.
pub async fn enumerate(
    root: &Path,
    manifest: Option<&Value>,
    loaded: Option<&Loaded>,
) -> Vec<Group> {
    let null = Value::Null;
    let field = |key: &str| manifest.map_or(&null, |m| &m[key]);
    let mut groups = vec![];
    let mut push = |kind, items: Items| {
        if !items.is_empty() {
            groups.push(Group { kind, items });
        }
    };
    push("agent", markdown(root, field("agents"), "agents").await);
    push(
        "command",
        markdown(root, field("commands"), "commands").await,
    );
    push("skill", skills(root, field("skills")).await);
    // 枚举阶段的诊断在 Node 中丢弃（发现阶段另行报告）。
    let mut ignored = vec![];
    let hooks = match loaded {
        Some(loaded) => {
            let sources = crate::hooks::sources(loaded, &mut ignored).await;
            crate::hooks::event_names(&sources, loaded, &mut ignored)
        }
        None => manifest
            .map(|m| inline_hook_events(&m["hooks"]))
            .unwrap_or_default(),
    };
    push("hook", hooks.into_iter().map(|n| (n, None)).collect());
    let mcp: Vec<String> = match loaded {
        Some(loaded) => crate::mcp::definitions(loaded, &mut ignored)
            .await
            .into_iter()
            .map(|(k, _)| k)
            .collect(),
        None => field("mcpServers")
            .as_object()
            .map(|o| o.keys().cloned().collect())
            .unwrap_or_default(),
    };
    push(
        "mcp",
        mcp.iter()
            .map(|n| crate::js::trim(n))
            .filter(|n| !n.is_empty())
            .map(|n| (n.to_owned(), None))
            .collect(),
    );
    groups
}
