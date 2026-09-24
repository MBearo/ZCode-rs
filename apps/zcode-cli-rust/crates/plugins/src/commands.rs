//! Component roots of enabled plugins and the generated command root (Node
//! `resolveComponentRoots`, `materializeCommandMetadataRoot`).
use crate::loaded::Loaded;
use crate::manifest::{Diagnostic, Severity};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// Node `resolveComponentRoots`: the default directory first when it exists,
/// then the manifest's paths; escaping paths are errors.
pub async fn roots(key: &str, loaded: &Loaded, diagnostics: &mut Vec<Diagnostic>) -> Vec<PathBuf> {
    let mut paths = crate::fsx::path_list(&loaded.manifest[key]);
    if crate::fsx::is_dir(&loaded.root.join(key)).await {
        paths.insert(0, key.to_owned());
    }
    let mut roots: Vec<PathBuf> = vec![];
    for raw in paths {
        let Some(path) = crate::fsx::resolve_inside(&loaded.root, &raw) else {
            diagnostics.push(
                Diagnostic::new(
                    "plugin_component_path_invalid",
                    Severity::Error,
                    format!("Plugin {key} path escapes plugin root: {raw}"),
                )
                .at(&loaded.manifest_path)
                .plugin(&loaded.id),
            );
            continue;
        };
        if !roots.contains(&path) {
            roots.push(path);
        }
    }
    roots
}

fn normalized_name(name: &str) -> Option<String> {
    let name = crate::js::trim(name).trim_start_matches('/').to_lowercase();
    let mut chars = name.chars();
    let valid = chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "_:-".contains(c));
    valid.then_some(name)
}

/// Node `stripMarkdownFrontmatter` (the body is rejoined with `\n`).
fn strip_frontmatter(markdown: &str) -> String {
    let normalized = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    if !normalized.starts_with("---") {
        return markdown.to_owned();
    }
    let lines: Vec<&str> = normalized
        .split('\n')
        .map(|l| l.strip_suffix('\r').unwrap_or(l))
        .collect();
    if crate::js::trim(lines[0]) != "---" {
        return markdown.to_owned();
    }
    match lines
        .iter()
        .enumerate()
        .skip(1)
        .find(|(_, line)| crate::js::trim(line) == "---")
    {
        Some((end, _)) => lines[end + 1..].join("\n"),
        None => markdown.to_owned(),
    }
}

/// Node `applyCommandMetadataFrontmatter`.
fn with_frontmatter(markdown: &str, metadata: &Value) -> String {
    let text = |key: &str| {
        metadata[key]
            .as_str()
            .map(|s| crate::js::trim(s).to_owned())
            .filter(|s| !s.is_empty())
    };
    let mut fields = vec![];
    for (key, name) in [
        ("description", "description"),
        ("argumentHint", "argument-hint"),
        ("model", "model"),
    ] {
        if let Some(value) = text(key) {
            fields.push(format!("{name}: {value}"));
        }
    }
    let tools: Vec<&str> = metadata["allowedTools"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(crate::js::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if !tools.is_empty() {
        fields.push(format!("allowed-tools: {}", tools.join(", ")));
    }
    if fields.is_empty() {
        return markdown.to_owned();
    }
    let body = strip_frontmatter(markdown);
    let body = body.trim_start_matches(crate::js::is_space);
    format!("---\n{}\n---\n\n{body}", fields.join("\n"))
}

/// Node `materializeCommandMetadataRoot`: manifest `commands` objects become
/// `data/generated-commands/<name>.md`; `true` when a command was written.
pub async fn materialize(
    loaded: &Loaded,
    data_path: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> bool {
    let Value::Object(spec) = &loaded.manifest["commands"] else {
        return false;
    };
    let generated = data_path.join("generated-commands");
    if tokio::fs::create_dir_all(&generated).await.is_err() {
        return false;
    }
    let invalid = |code: &'static str, message: String, path: &Path| {
        Diagnostic::new(code, Severity::Error, message)
            .at(path)
            .plugin(&loaded.id)
    };
    let mut wrote = false;
    for (raw, metadata) in spec {
        let manifest = &loaded.manifest_path;
        if !metadata.is_object() {
            let message = format!("Plugin command metadata must be an object: {raw}");
            diagnostics.push(invalid("plugin_manifest_invalid", message, manifest));
            continue;
        }
        let Some(name) = normalized_name(raw) else {
            let message = format!("Invalid plugin command name: {raw}");
            diagnostics.push(invalid("plugin_manifest_invalid", message, manifest));
            continue;
        };
        let (source, content) = (metadata["source"].as_str(), metadata["content"].as_str());
        let markdown = match (
            source.filter(|s| !s.is_empty()),
            content.filter(|c| !c.is_empty()),
        ) {
            (Some(_), Some(_)) | (None, None) => {
                let message =
                    format!("Plugin command '{raw}' must provide exactly one of source or content");
                diagnostics.push(invalid("plugin_manifest_invalid", message, manifest));
                continue;
            }
            (None, Some(content)) => content.to_owned(),
            (Some(source), None) => {
                let relative = source.strip_prefix("./").unwrap_or(source);
                let Some(path) = crate::fsx::resolve_inside(&loaded.root, relative) else {
                    let message = format!("Plugin command source escapes plugin root: {source}");
                    diagnostics.push(invalid("plugin_component_path_invalid", message, manifest));
                    continue;
                };
                if !crate::fsx::is_file(&path).await {
                    let message = format!("Plugin command source file not found: {source}");
                    diagnostics.push(invalid("plugin_component_path_invalid", message, &path));
                    continue;
                }
                match crate::fsx::read_text(&path).await {
                    Ok(text) => text,
                    Err(_) => continue,
                }
            }
        };
        let body = with_frontmatter(&markdown, metadata);
        if tokio::fs::write(generated.join(format!("{name}.md")), body)
            .await
            .is_ok()
        {
            wrote = true;
        }
    }
    wrote
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn generated_commands_follow_node_frontmatter() {
        assert_eq!(normalized_name(" /Review:PR "), Some("review:pr".into()));
        assert_eq!(normalized_name("-x"), None);
        let metadata = json!({"description":" Review ","allowedTools":["Read"," ",1,"Bash"]});
        assert_eq!(
            with_frontmatter("---\nold: 1\n---\n\n  Body", &metadata),
            "---\ndescription: Review\nallowed-tools: Read, Bash\n---\n\nBody"
        );
        assert_eq!(with_frontmatter("Body", &json!({})), "Body");
    }
}
