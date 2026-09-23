//! One configuration file: parse, legacy normalization, schema validation and
//! conversion to a runtime patch (Node `schema.ts`, `file-config.adapter.ts`,
//! `project-config.adapter.ts`).
use super::Diagnostic;
use crate::protocol::json_schema::Node;
use serde_json::{Map, Value, json};
use std::{path::Path, sync::OnceLock};

const SCHEMA: &str = include_str!("../../schema/config.json");
const LEGACY_CUA_PLUGIN_ID: &str = "zcode-cua@zcode-plugins-official";
const CANONICAL_CUA_PLUGIN_ID: &str = "computer-use@zcode-plugins-official";

struct Schemas {
    file: Node,
    mcp_server: Node,
}

fn schemas() -> &'static Schemas {
    static SCHEMAS: OnceLock<Schemas> = OnceLock::new();
    SCHEMAS.get_or_init(|| {
        let schema: Value = serde_json::from_str(SCHEMA).expect("generated config schema is JSON");
        let compile =
            |value: &Value| Node::compile(value).expect("config schema uses the supported subset");
        Schemas {
            file: compile(&schema["file"]),
            mcp_server: compile(&schema["mcpServer"]),
        }
    })
}

/// Result of loading one file (Node `LoadedConfig`).
#[derive(Clone, Debug, PartialEq)]
pub struct LoadedFile {
    pub path: String,
    pub loaded: bool,
    pub patch: Map<String, Value>,
    pub diagnostics: Vec<Diagnostic>,
}

fn invalid(path: &str, message: String) -> LoadedFile {
    LoadedFile {
        path: path.into(),
        loaded: false,
        patch: Map::new(),
        diagnostics: vec![Diagnostic {
            code: "config_file_invalid",
            file_path: Some(path.into()),
            message,
            path: None,
            severity: "error",
        }],
    }
}

/// Load one config file. `content` is `Ok(None)` when the file does not exist.
pub fn parse_file(path: &str, content: Result<Option<&str>, String>) -> LoadedFile {
    let text = match content {
        Ok(None) => {
            return LoadedFile {
                path: path.into(),
                loaded: false,
                patch: Map::new(),
                diagnostics: vec![],
            };
        }
        Ok(Some(text)) => text,
        Err(error) => return invalid(path, error),
    };
    let value: Value = match serde_json::from_str(text) {
        Ok(value) => value,
        Err(error) => return invalid(path, format!("Invalid JSON: {error}")),
    };
    let mut diagnostics = vec![];
    let normalized = normalize_file(value, &mut diagnostics);
    if let Err(issue) = schemas().file.validate(&normalized, "") {
        return invalid(path, issue);
    }
    for diagnostic in &mut diagnostics {
        diagnostic.file_path = Some(path.into());
    }
    // 与 zod 一致：转换前先按 schema 剥离非 strict 对象里的未知字段。
    let parsed = schemas().file.strip(&normalized);
    LoadedFile {
        path: path.into(),
        loaded: true,
        patch: to_patch(parsed.as_object().expect("schema requires an object")),
        diagnostics,
    }
}

/// Node `normalizeMcpServerConfigInput`.
fn normalize_mcp_server(value: &Value) -> Value {
    let Some(input) = value.as_object() else {
        return value.clone();
    };
    let mut server = input.clone();
    if !server.contains_key("env")
        && let Some(environment) = server.get("environment").cloned()
    {
        server.insert("env".into(), environment);
    }
    server.remove("environment");
    let enable = server.get("enable").and_then(Value::as_bool);
    let enabled = server.get("enabled").and_then(Value::as_bool);
    if enable.is_some() || enabled.is_some() {
        let value = if enable == Some(false) {
            false
        } else {
            enabled.unwrap_or(true)
        };
        server.insert("enabled".into(), value.into());
    }
    server.remove("enable");
    server.remove("timeout");
    server.remove("startup_timeout_sec");
    let nonblank = |key: &str| {
        server
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    if server.get("type").and_then(Value::as_str) == Some("remote") {
        server.insert("type".into(), "http".into());
    } else if !server.get("type").is_some_and(Value::is_string) {
        if nonblank("command") {
            server.insert("type".into(), "stdio".into());
        } else if nonblank("url") {
            server.insert("type".into(), "http".into());
        }
    }
    let remote = matches!(
        server.get("type").and_then(Value::as_str),
        Some("http" | "sse")
    );
    if remote
        && server.get("headers").is_none()
        && let Some(headers) = server.get("http_headers").cloned()
    {
        server.insert("headers".into(), headers);
    }
    server.remove("http_headers");
    Value::Object(server)
}

/// Node `normalizeConfigFileInput`: invalid MCP servers are dropped one by one.
fn normalize_file(value: Value, diagnostics: &mut Vec<Diagnostic>) -> Value {
    let Value::Object(mut root) = value else {
        return value;
    };
    let Some(Value::Object(mut mcp)) = root.get("mcp").cloned() else {
        return Value::Object(root);
    };
    let Some(servers) = mcp.get("servers").cloned() else {
        return Value::Object(root);
    };
    let Value::Object(servers) = servers else {
        diagnostics.push(Diagnostic {
            code: "config_mcp_server_invalid",
            file_path: None,
            message: "mcp.servers must be a JSON object; ignoring all MCP servers.".into(),
            path: Some("mcp.servers".into()),
            severity: "warning",
        });
        mcp.insert("servers".into(), json!({}));
        root.insert("mcp".into(), Value::Object(mcp));
        return Value::Object(root);
    };
    let mut parsed = Map::new();
    for (name, server) in servers {
        let normalized = normalize_mcp_server(&server);
        match schemas().mcp_server.validate(&normalized, "") {
            Ok(()) => {
                parsed.insert(name, normalized);
            }
            Err(issue) => diagnostics.push(Diagnostic {
                code: "config_mcp_server_invalid",
                file_path: None,
                message: issue,
                path: Some(format!("mcp.servers.{name}")),
                severity: "warning",
            }),
        }
    }
    mcp.insert("servers".into(), Value::Object(parsed));
    root.insert("mcp".into(), Value::Object(mcp));
    Value::Object(root)
}

fn is_absolute_config_path(path: &str) -> bool {
    path.starts_with('/')
        || path.starts_with('\\')
        || (path.len() >= 3
            && path.as_bytes()[0].is_ascii_alphabetic()
            && path.as_bytes()[1] == b':'
            && matches!(path.as_bytes()[2], b'\\' | b'/'))
}

fn canonical_plugin(id: &str) -> &str {
    if id == LEGACY_CUA_PLUGIN_ID {
        CANONICAL_CUA_PLUGIN_ID
    } else {
        id
    }
}

/// Node `normalizePluginConfig`.
fn normalize_plugins(plugins: &Map<String, Value>) -> Value {
    let mut result = plugins.clone();
    for key in ["enabledPlugins", "options"] {
        if let Some(Value::Object(map)) = plugins.get(key) {
            let mut map = map.clone();
            if let Some(legacy) = map.remove(LEGACY_CUA_PLUGIN_ID) {
                map.entry(CANONICAL_CUA_PLUGIN_ID).or_insert(legacy);
            }
            result.insert(key.into(), Value::Object(map));
        }
    }
    if let Some(Value::Array(ids)) = plugins.get("suppressedBuiltins") {
        let mut canonical: Vec<Value> = vec![];
        for id in ids.iter().filter_map(Value::as_str).map(canonical_plugin) {
            if id == CANONICAL_CUA_PLUGIN_ID && canonical.iter().any(|v| v == id) {
                continue;
            }
            canonical.push(id.into());
        }
        result.insert("suppressedBuiltins".into(), Value::Array(canonical));
    }
    Value::Object(result)
}

/// Node `parsedConfigFileToRuntimePatch`.
fn to_patch(file: &Map<String, Value>) -> Map<String, Value> {
    let mut patch = Map::new();
    let mut copy = |from: &str, to: &str| {
        if let Some(value) = file.get(from).filter(|v| !v.is_null()) {
            patch.insert(to.into(), value.clone());
        }
    };
    for key in [
        "modelStream",
        "permission",
        "storage",
        "network",
        "features",
        "memory",
        "mcp",
    ] {
        copy(key, key);
    }
    if let Some(Value::Object(plugins)) = file.get("plugins") {
        patch.insert("plugins".into(), normalize_plugins(plugins));
    }
    let skills = file.get("skills").and_then(Value::as_object);
    if let Some(skills) = skills {
        let runtime: Map<String, Value> =
            ["enabled", "includeInstructions", "metadataBudget", "roots"]
                .into_iter()
                .filter_map(|key| skills.get(key).map(|v| (key.to_owned(), v.clone())))
                .collect();
        if !runtime.is_empty() {
            patch.insert("skills".into(), Value::Object(runtime));
        }
    }
    let mut overrides = Map::new();
    if let Some(Value::Object(skill)) = file.get("skill") {
        overrides.extend(skill.clone());
    }
    if let Some(skills) = skills {
        for (path, value) in skills {
            let valid = value
                .as_object()
                .is_some_and(|o| o.get("enable").is_none_or(Value::is_boolean));
            if is_absolute_config_path(path) && valid {
                let enable = value.get("enable").cloned();
                let mut entry = Map::new();
                if let Some(enable) = enable {
                    entry.insert("enable".into(), enable);
                }
                overrides.insert(path.clone(), Value::Object(entry));
            }
        }
    }
    if !overrides.is_empty() {
        patch.insert("skillOverrides".into(), Value::Object(overrides));
    }
    let mut copy = |from: &str, to: &str| {
        if let Some(value) = file.get(from).filter(|v| !v.is_null()) {
            patch.insert(to.into(), value.clone());
        }
    };
    copy("command", "commandOverrides");
    for key in [
        "logging",
        "ui",
        "toolConcurrency",
        "modelAnomalyGuard",
        "hooks",
    ] {
        copy(key, key);
    }
    patch
}

/// Lexical `path.resolve(base, relative)` without touching the filesystem.
pub(super) fn resolve_path(base: &str, relative: &str) -> String {
    let joined = if Path::new(relative).is_absolute() {
        Path::new(relative).to_path_buf()
    } else {
        Path::new(base).join(relative)
    };
    let mut parts: Vec<std::path::Component> = vec![];
    for part in joined.components() {
        match part {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if matches!(parts.last(), Some(std::path::Component::Normal(_))) {
                    parts.pop();
                }
            }
            other => parts.push(other),
        }
    }
    parts
        .iter()
        .collect::<std::path::PathBuf>()
        .to_string_lossy()
        .into_owned()
}

/// Node `loadProjectConfigFile` post-processing: hooks leave the executable
/// patch (kept as a trust candidate) and relative stdio MCP `cwd` resolves
/// against the project base directory.
pub fn project_file(mut file: LoadedFile) -> (LoadedFile, Option<Value>) {
    if !file.loaded {
        return (file, None);
    }
    let path = Path::new(&file.path);
    let parent = path.parent().unwrap_or(Path::new(""));
    let base = if parent.file_name().is_some_and(|n| n == ".zcode") {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    }
    .to_string_lossy()
    .into_owned();
    let hooks = file.patch.remove("hooks");
    if hooks.is_some() {
        file.diagnostics.push(Diagnostic {
            code: "config_project_hooks_pending_trust",
            file_path: Some(file.path.clone()),
            message: "Project hooks are pending workspace trust and remain blocked".into(),
            path: Some("hooks".into()),
            severity: "warning",
        });
    }
    if let Some(Value::Object(servers)) = file
        .patch
        .get_mut("mcp")
        .and_then(|mcp| mcp.get_mut("servers"))
    {
        for server in servers.values_mut() {
            if server["type"] == "stdio" {
                let cwd = server["cwd"].as_str().unwrap_or(".").to_owned();
                server["cwd"] = resolve_path(&base, &cwd).into();
            }
        }
    }
    (file, hooks)
}
