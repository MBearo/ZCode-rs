use super::{extension_config as config, extension_plugins as plugins};
use crate::domain::subagent::{Profile, builtins};
use anyhow::Result;
use serde_json::json;
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;

pub(super) async fn discover(cwd: &Path, cancel: &CancellationToken) -> Result<Vec<Profile>> {
    let config = config::load(cwd).await?;
    let root = std::env::var("ZCODE_STORAGE_DIR")
        .ok()
        .or_else(|| config["storage"]["dir"].as_str().map(str::to_owned))
        .map(|p| config::resolve(&config::home(), &p))
        .unwrap_or_else(|| config::home().join(".zcode"));
    let state = config::json_file(&root.join("v2/agents-state.json")).await?;
    let mut result = builtins()
        .into_iter()
        .map(|mut p| {
            if let Some(selection) = state["builtInModelSelectionOverrides"].get(&p.name) {
                p.model_selection = Some(selection.clone());
            }
            (p.name.clone(), p)
        })
        .collect::<BTreeMap<_, _>>();
    for (dir, source) in [
        (root.join("agents"), "user"),
        (cwd.join(".zcode/agents"), "project"),
    ] {
        for path in markdown(&dir, cancel).await? {
            if let Some(profile) = read(&path, source, cancel).await? {
                let disabled = source == "user"
                    && state["disabledAgentIds"].as_array().is_some_and(|ids| {
                        ids.contains(&json!(format!("user:user:{}", profile.name.to_lowercase())))
                    });
                if !disabled {
                    result.insert(profile.name.clone(), profile);
                }
            }
        }
    }
    let mut imported = vec![];
    let mut counts = BTreeMap::<String, usize>::new();
    for plugin in plugins::enabled(cwd, &config, cancel).await? {
        for path in markdown(&plugin.root.join("agents"), cancel).await? {
            if !plugins::contained_file(&plugin.root, &path).await {
                continue;
            }
            if let Some(mut p) = read(&path, "plugin", cancel).await? {
                let bare = p.name.clone();
                if let Some(selection) = state["pluginAgentModelSelectionOverrides"]
                    .get(format!("plugin:{}:{}", plugin.id, bare).as_str())
                {
                    p.model_selection = Some(selection.clone());
                }
                *counts.entry(bare.clone()).or_default() += 1;
                p.name = format!("{}:{}", plugin.name, p.name);
                p.system_prompt = p
                    .system_prompt
                    .replace("${CLAUDE_PLUGIN_ROOT}", &plugin.root.to_string_lossy())
                    .replace("${ZCODE_PLUGIN_ROOT}", &plugin.root.to_string_lossy());
                imported.push((bare, p));
            }
        }
    }
    for (bare, p) in imported {
        if counts[&bare] == 1 && !result.contains_key(&bare) {
            let mut alias = p.clone();
            alias.name = bare.clone();
            result.insert(bare, alias);
        }
        result.insert(p.name.clone(), p);
    }
    Ok(result.into_values().collect())
}
async fn markdown(root: &Path, cancel: &CancellationToken) -> Result<Vec<PathBuf>> {
    let mut stack = vec![root.to_owned()];
    let mut files = vec![];
    let mut seen = 0;
    while let Some(path) = stack.pop() {
        super::tools::check_cancel(cancel)?;
        let mut dir = match tokio::fs::read_dir(&path).await {
            Ok(dir) => dir,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        while let Some(entry) = dir.next_entry().await? {
            seen += 1;
            anyhow::ensure!(seen <= 10000, "Agent profile discovery exceeds limit");
            let ty = entry.file_type().await?;
            if ty.is_dir() {
                stack.push(entry.path());
            } else if ty.is_file()
                && entry.path().extension().is_some_and(|s| {
                    s.eq_ignore_ascii_case("md") || s.eq_ignore_ascii_case("markdown")
                })
            {
                files.push(entry.path());
            }
        }
    }
    files.sort();
    Ok(files)
}
async fn read(path: &Path, source: &str, cancel: &CancellationToken) -> Result<Option<Profile>> {
    let mut bytes = vec![];
    tokio::fs::File::open(path)
        .await?
        .take(128 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await?;
    super::tools::check_cancel(cancel)?;
    if bytes.len() > 128 * 1024 {
        return Ok(None);
    }
    let Ok(text) = String::from_utf8(bytes) else {
        return Ok(None);
    };
    Ok(crate::domain::agent_profile::parse(&text, source))
}
pub(super) async fn memory(
    cwd: &Path,
    profile: &Profile,
    cancel: &CancellationToken,
) -> Result<Option<String>> {
    let Some(scope) = &profile.memory else {
        return Ok(None);
    };
    anyhow::ensure!(
        ["user", "project", "local"].contains(&scope.as_str()),
        "Invalid profile memory scope"
    );
    let config = config::load(cwd).await?;
    if config["features"]["memory"] == false || config["memory"]["use"] == false {
        return Ok(None);
    }
    let storage = std::env::var("ZCODE_STORAGE_DIR")
        .ok()
        .or_else(|| config["storage"]["dir"].as_str().map(str::to_owned))
        .map(|p| config::resolve(&config::home(), &p))
        .unwrap_or_else(|| config::home().join(".zcode"));
    let key = profile
        .name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();
    let root = match scope.as_str() {
        "user" => storage.join("agent-memory"),
        "project" => cwd.join(".zcode/agent-memory"),
        _ => cwd.join(".zcode/agent-memory-local"),
    }
    .join(key);
    super::tools::check_cancel(cancel)?;
    tokio::fs::create_dir_all(&root).await?;
    let mut bytes = vec![];
    if let Ok(file) = tokio::fs::File::open(root.join("MEMORY.md")).await {
        file.take(128 * 1024).read_to_end(&mut bytes).await?;
    }
    let index = String::from_utf8_lossy(&bytes)
        .lines()
        .take(200)
        .collect::<Vec<_>>()
        .join("\n");
    let templates: serde_json::Value =
        serde_json::from_str(include_str!("agent_memory_templates.json"))?;
    Ok(Some(
        templates[scope]
            .as_str()
            .unwrap()
            .replace("{memoryRoot}", &root.to_string_lossy())
            .replace(
                "{memoryIndex}",
                if index.is_empty() {
                    "No existing memory index."
                } else {
                    &index
                },
            ),
    ))
}
