//! Plugin diagnostics and `plugin.json` loading (Node `readManifest`,
//! `findManifest`, `normalizeAuthorValue`).
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// Node `PluginDiagnostic`.
#[derive(Clone, Debug, PartialEq)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
    pub path: Option<PathBuf>,
    pub plugin_id: Option<String>,
    pub severity: Severity,
}

impl Diagnostic {
    pub fn new(code: &'static str, severity: Severity, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            path: None,
            plugin_id: None,
            severity,
        }
    }

    pub fn at(mut self, path: impl Into<PathBuf>) -> Self {
        self.path = Some(path.into());
        self
    }

    pub fn plugin(mut self, id: &str) -> Self {
        self.plugin_id = Some(id.to_owned());
        self
    }

    /// Protocol `ZCodePluginDiagnostic`: the path is not part of it.
    pub fn protocol(&self) -> Value {
        let mut value = json!({"code":self.code,"message":self.message,
            "severity":if self.severity == Severity::Error {"error"} else {"warning"}});
        if let Some(id) = &self.plugin_id {
            value["pluginId"] = id.clone().into();
        }
        value
    }
}

pub const DEFAULT_VERSION: &str = "0.0.0";
const MANIFEST_DIRS: [&str; 3] = [".zcode-plugin", ".claude-plugin", ".codex-plugin"];

/// Node `PLUGIN_NAME_PATTERN` and `MARKETPLACE_NAME_PATTERN`: `^[a-z0-9][a-z0-9._-]{0,127}$`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 128
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "._-".contains(c))
}

/// Node `findManifest`: the first `plugin.json` among the three conventions.
pub async fn find(root: &Path) -> Option<PathBuf> {
    for dir in MANIFEST_DIRS {
        let path = root.join(dir).join("plugin.json");
        if crate::fsx::is_file(&path).await {
            return Some(path);
        }
    }
    None
}

/// Node `readManifest`: the manifest with its trimmed name and a default
/// version, or the reason it is invalid.
pub async fn read(path: &Path) -> Result<Value, String> {
    let parsed = match crate::fsx::read_json(path).await {
        Ok(Ok(value)) => value,
        Ok(Err(message)) => return Err(message),
        Err(error) => return Err(error.to_string()),
    };
    let Value::Object(mut manifest) = parsed else {
        return Err("Manifest must be a JSON object".into());
    };
    let name = manifest
        .get("name")
        .and_then(Value::as_str)
        .map(|n| crate::js::trim(n).to_owned())
        .unwrap_or_default();
    if !valid_name(&name) {
        return Err(format!("Invalid plugin name: {name}"));
    }
    manifest.insert("name".into(), name.into());
    if !manifest.get("version").is_some_and(Value::is_string) {
        manifest.insert("version".into(), DEFAULT_VERSION.into());
    }
    Ok(Value::Object(manifest))
}

/// Node `normalizeAuthorValue`: `(name, url)` from a string or `{name, url}`.
pub fn author(value: &Value) -> Option<(Option<String>, Option<String>)> {
    let field = |key: &str| {
        value[key]
            .as_str()
            .map(crate::js::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
    };
    match value {
        Value::String(name) => {
            let name = crate::js::trim(name);
            (!name.is_empty()).then(|| (Some(name.to_owned()), None))
        }
        Value::Object(_) => {
            let (name, url) = (field("name"), field("url"));
            (name.is_some() || url.is_some()).then_some((name, url))
        }
        _ => None,
    }
}

/// A non-empty (after trim) string field, kept verbatim.
pub fn text(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| !crate::js::trim(s).is_empty())
        .map(str::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn manifests_follow_node_precedence_and_validation() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        assert_eq!(find(root).await, None);
        for (sub, name) in [(".codex-plugin", "c"), (".claude-plugin", "b")] {
            tokio::fs::create_dir_all(root.join(sub)).await.unwrap();
            tokio::fs::write(
                root.join(sub).join("plugin.json"),
                json!({ "name": name }).to_string(),
            )
            .await
            .unwrap();
        }
        let path = find(root).await.unwrap();
        assert!(path.ends_with(".claude-plugin/plugin.json"));
        let manifest = read(&path).await.unwrap();
        assert_eq!(manifest["version"], "0.0.0");
        tokio::fs::write(&path, r#"{"name":" Bad "}"#)
            .await
            .unwrap();
        assert_eq!(read(&path).await.unwrap_err(), "Invalid plugin name: Bad");
        tokio::fs::write(&path, "[1]").await.unwrap();
        assert_eq!(
            read(&path).await.unwrap_err(),
            "Manifest must be a JSON object"
        );
        assert!(valid_name("a.b-c_9") && !valid_name("-a") && !valid_name(&"a".repeat(129)));
        assert_eq!(
            author(&json!({"name":" Z ","url":""})),
            Some((Some("Z".into()), None))
        );
        assert_eq!(author(&json!("  ")), None);
    }
}
