//! Asynchronous filesystem helpers with the Node plugin adapter's semantics
//! (`adapters/src/plugins/helpers.ts`).
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

/// Largest JSON document read from plugin storage or a plugin root (Node
/// `MARKETPLACE_JSON_MAX_BYTES`); larger files read as invalid.
pub const MAX_JSON_BYTES: u64 = 10 * 1024 * 1024;

/// Node `statSync(path).isDirectory()` (follows links).
pub async fn is_dir(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok_and(|m| m.is_dir())
}

/// Node `statSync(path).isFile()` (follows links).
pub async fn is_file(path: &Path) -> bool {
    tokio::fs::metadata(path).await.is_ok_and(|m| m.is_file())
}

pub async fn exists(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path).await.is_ok()
}

/// Node `isMissingPath`: only "not found" and "not a directory" count as missing.
pub async fn is_missing(path: &Path) -> bool {
    match tokio::fs::metadata(path).await {
        Ok(_) => false,
        Err(error) => {
            error.kind() == std::io::ErrorKind::NotFound
                || error.kind() == std::io::ErrorKind::NotADirectory
        }
    }
}

pub async fn is_symlink(path: &Path) -> bool {
    tokio::fs::symlink_metadata(path)
        .await
        .is_ok_and(|m| m.file_type().is_symlink())
}

/// Node `path.resolve`: `path` against `base`, with `.` and `..` removed lexically.
pub fn resolve(base: &Path, path: &str) -> PathBuf {
    normalize(&base.join(path))
}

pub fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !matches!(
                    result.components().next_back(),
                    None | Some(Component::RootDir | Component::Prefix(_))
                ) {
                    result.pop();
                }
            }
            other => result.push(other),
        }
    }
    result
}

/// Node `resolveInside`: a relative `raw` that stays inside `root`. Like
/// Node, a first segment merely starting with `..` (such as `..cache`) is
/// also rejected.
pub fn resolve_inside(root: &Path, raw: &str) -> Option<PathBuf> {
    if Path::new(raw).is_absolute() || raw.starts_with('/') || raw.starts_with('\\') {
        return None;
    }
    let resolved = resolve(root, raw);
    let relative = resolved.strip_prefix(normalize(root)).ok()?;
    let text = relative.to_string_lossy();
    (text.is_empty() || !text.starts_with("..")).then_some(resolved)
}

/// Node `sanitizePluginId`.
pub fn sanitize_id(id: &str) -> String {
    id.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || "_.@-".contains(c) {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Node `parsePathList`: a string or the strings of an array.
pub fn path_list(value: &Value) -> Vec<String> {
    match value {
        Value::String(s) => vec![s.clone()],
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str().map(str::to_owned))
            .collect(),
        _ => vec![],
    }
}

/// UTF-8 text of a file, `Err` with the IO error kind's message.
pub async fn read_text(path: &Path) -> std::io::Result<String> {
    let metadata = tokio::fs::metadata(path).await?;
    if metadata.len() > MAX_JSON_BYTES {
        return Err(std::io::Error::other(format!(
            "File exceeds {MAX_JSON_BYTES} bytes: {}",
            path.display()
        )));
    }
    let bytes = tokio::fs::read(path).await?;
    String::from_utf8(bytes).map_err(|e| std::io::Error::other(e.to_string()))
}

/// A JSON file parsed with `JSON.parse` semantics; `Err` carries the message
/// Node reports for read or parse failures.
pub async fn read_json(path: &Path) -> std::io::Result<Result<Value, String>> {
    let text = read_text(path).await?;
    // JSON.parse 允许 UTF-8 BOM 之外的内容；BOM 在 Node readFileSync("utf8") 后仍保留并导致失败。
    Ok(serde_json::from_str(&text).map_err(|e| e.to_string()))
}

/// Node `readJsonFileSync` without atomic recovery: any failure is `None`.
pub async fn read_json_lenient(path: &Path) -> Option<Value> {
    read_json(path).await.ok()?.ok()
}

pub async fn realpath_or_self(path: &Path) -> PathBuf {
    tokio::fs::canonicalize(path)
        .await
        .unwrap_or_else(|_| path.to_owned())
}

/// Directory names under `path` in sorted order (Node `readdirSync` order is
/// the platform's; sorting makes discovery deterministic across platforms).
pub async fn read_dir_names(path: &Path) -> std::io::Result<Vec<(String, std::fs::FileType)>> {
    let mut entries = tokio::fs::read_dir(path).await?;
    let mut names = vec![];
    while let Some(entry) = entries.next_entry().await? {
        names.push((
            entry.file_name().to_string_lossy().into_owned(),
            entry.file_type().await?,
        ));
    }
    names.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inside_paths_follow_node() {
        let root = Path::new("/p");
        assert_eq!(
            resolve_inside(root, "./skills/a"),
            Some(PathBuf::from("/p/skills/a"))
        );
        assert_eq!(resolve_inside(root, "."), Some(PathBuf::from("/p")));
        assert_eq!(resolve_inside(root, "../x"), None);
        assert_eq!(resolve_inside(root, "/abs"), None);
        assert_eq!(resolve_inside(root, "..cache"), None, "Node quirk");
        assert_eq!(resolve_inside(root, "a/../../x"), None);
        assert_eq!(sanitize_id("a b/c@m"), "a-b-c@m");
        assert_eq!(path_list(&serde_json::json!(["a", 1, "b"])), vec!["a", "b"]);
    }
}
