//! Plugin MCP server declarations and their resolution into server configs
//! (Node `adapters/src/plugins/mcp.ts`).
use crate::loaded::Loaded;
use crate::manifest::{Diagnostic, Severity};
use serde_json::{Map, Value, json};
use std::path::Path;

/// `(key, raw config)` with JS object-spread order: a later key replaces the
/// value but keeps the first position.
pub type Definitions = Vec<(String, Value)>;

fn spread(target: &mut Definitions, source: Definitions) {
    for (key, value) in source {
        match target.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => target.push((key, value)),
        }
    }
}

fn shape(value: &Value, loaded: &Loaded, diagnostics: &mut Vec<Diagnostic>) -> Definitions {
    let Value::Object(object) = value else {
        diagnostics.push(
            Diagnostic::new(
                "plugin_mcp_invalid",
                Severity::Error,
                "Plugin MCP config must be an object",
            )
            .at(&loaded.manifest_path)
            .plugin(&loaded.id),
        );
        return vec![];
    };
    let servers = match object.get("mcpServers") {
        Some(Value::Object(servers)) => servers,
        _ => object,
    };
    servers
        .iter()
        .filter(|(_, config)| config.is_object())
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

async fn from_file(path: &Path, loaded: &Loaded, diagnostics: &mut Vec<Diagnostic>) -> Definitions {
    let failure = |message: String| {
        Diagnostic::new("plugin_mcp_read_failed", Severity::Error, message)
            .at(path)
            .plugin(&loaded.id)
    };
    match crate::fsx::read_json(path).await {
        Ok(Ok(value)) => shape(&value, loaded, diagnostics),
        Ok(Err(message)) => {
            diagnostics.push(failure(message));
            vec![]
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => vec![],
        Err(error) => {
            diagnostics.push(failure(error.to_string()));
            vec![]
        }
    }
}

async fn from_spec(
    spec: &Value,
    loaded: &Loaded,
    diagnostics: &mut Vec<Diagnostic>,
) -> Definitions {
    match spec {
        Value::Null => vec![],
        Value::String(raw) => match crate::fsx::resolve_inside(&loaded.root, raw) {
            Some(path) => from_file(&path, loaded, diagnostics).await,
            None => {
                diagnostics.push(
                    Diagnostic::new(
                        "plugin_component_path_invalid",
                        Severity::Error,
                        format!("Plugin mcpServers path escapes plugin root: {raw}"),
                    )
                    .at(&loaded.manifest_path)
                    .plugin(&loaded.id),
                );
                vec![]
            }
        },
        Value::Array(items) => {
            let mut merged = vec![];
            for item in items {
                let next = Box::pin(from_spec(item, loaded, diagnostics)).await;
                spread(&mut merged, next);
            }
            merged
        }
        other => shape(other, loaded, diagnostics),
    }
}

/// Node `loadPluginMcpServerDefinitions`: `.mcp.json`, then `manifest.mcpServers`.
pub async fn definitions(loaded: &Loaded, diagnostics: &mut Vec<Diagnostic>) -> Definitions {
    let mut merged = from_file(&loaded.root.join(".mcp.json"), loaded, diagnostics).await;
    let manifest = from_spec(&loaded.manifest["mcpServers"], loaded, diagnostics).await;
    spread(&mut merged, manifest);
    merged
}

/// Inputs of `${...}` expansion (Node `VariableContext`).
pub struct Context<'a> {
    pub loaded: &'a Loaded,
    pub data_path: &'a Path,
    pub cwd: &'a Path,
    pub env: &'a (dyn Fn(&str) -> Option<String> + Sync),
    pub options: &'a Map<String, Value>,
}

enum Failure {
    Variable(String),
    Other(String),
}

fn option_text(value: &Value) -> Option<String> {
    match value {
        Value::String(s) => Some(s.clone()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Number(n) => Some(zcode_cli_domain::hooks::digest::js_number(n.as_f64()?)),
        _ => None,
    }
}

fn variable(name: &str, context: &Context<'_>, sensitive: bool) -> Result<Option<String>, Failure> {
    let path = |p: &Path| Ok(Some(p.to_string_lossy().into_owned()));
    match name {
        "CLAUDE_PLUGIN_ROOT" | "ZCODE_PLUGIN_ROOT" => return path(&context.loaded.root),
        "CLAUDE_PLUGIN_DATA" | "ZCODE_PLUGIN_DATA" => return path(context.data_path),
        "CLAUDE_PROJECT_DIR" | "ZCODE_PROJECT_DIR" => return path(context.cwd),
        "CLAUDE_CODE_SESSION_ID" | "CLAUDE_SESSION_ID" | "ZCODE_SESSION_ID" => {
            return Err(Failure::Variable(format!(
                "Plugin variable requires a runtime session context: {name}"
            )));
        }
        "CLAUDE_SKILL_DIR" | "ZCODE_SKILL_DIR" => {
            return Err(Failure::Variable(format!(
                "Plugin variable requires a skill context: {name}"
            )));
        }
        _ => {}
    }
    if let Some(key) = name.strip_prefix("user_config.") {
        let option = &context.loaded.manifest["userConfig"][key];
        if option["sensitive"] == true && !sensitive {
            return Err(Failure::Variable(format!(
                "Sensitive plugin user_config value cannot be used in this field: {key}"
            )));
        }
        let default = &option["default"];
        return context
            .options
            .get(key)
            .and_then(option_text)
            .or_else(|| option_text(default))
            .map(Some)
            .ok_or_else(|| Failure::Variable(format!("Missing plugin user_config value: {key}")));
    }
    let env_name = {
        let mut chars = name.chars();
        chars
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if name.starts_with("ZCODE_") || (sensitive && env_name) {
        // 敏感出口才允许展开任意环境变量，避免 secret 进入 args、URL 等可见字段。
        return (context.env)(name)
            .map(Some)
            .ok_or_else(|| Failure::Variable(format!("Missing environment variable: {name}")));
    }
    Ok(None)
}

/// Node `resolveTemplate`: `${name}` occurrences replaced left to right.
fn template(value: &str, context: &Context<'_>, sensitive: bool) -> Result<String, Failure> {
    let mut out = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        let Some(end) = after.find('}').filter(|e| *e > 0) else {
            // `${}` 或未闭合：正则不匹配，原样保留并继续向后查找。
            out.push_str(&rest[..start + 2]);
            rest = after;
            continue;
        };
        out.push_str(&rest[..start]);
        let name = &after[..end];
        match variable(name, context, sensitive)? {
            Some(text) => out.push_str(&text),
            None => out.push_str(&rest[start..start + 2 + end + 1]),
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

fn string_record(value: &Value, context: &Context<'_>) -> Result<Map<String, Value>, Failure> {
    let mut out = Map::new();
    for (key, value) in value.as_object().into_iter().flatten() {
        if let Some(text) = value.as_str() {
            out.insert(key.clone(), template(text, context, true)?.into());
        }
    }
    Ok(out)
}

fn required<'v>(value: &'v Value, message: &str) -> Result<&'v str, Failure> {
    value
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| Failure::Other(message.into()))
}

fn server(key: &str, raw: &Value, context: &Context<'_>) -> Result<Value, Failure> {
    let kind = match raw["type"].as_str() {
        Some(kind) => kind,
        None if raw["command"].is_string() => "stdio",
        None => "http",
    };
    if !["stdio", "http", "sse"].contains(&kind) {
        return Err(Failure::Other(format!("Unsupported MCP transport: {kind}")));
    }
    // zcode_official 只允许 http 与 stdio；sse 出现即禁用，不静默忽略。
    let auth = crate::mcp_auth::parse(raw.get("auth"), key).map_err(Failure::Other)?;
    if auth.is_some() && kind != "http" && kind != "stdio" {
        return Err(Failure::Other(format!(
            "MCP server {key}: zcode_official auth requires type \"http\" or \"stdio\", got \"{kind}\""
        )));
    }
    let oauth_conflict = || {
        let message = match auth {
            Some(_) => {
                format!("MCP server {key}: zcode_official auth cannot be combined with oauth")
            }
            // Rust MCP 没有 OAuth 通道；没有鉴权通道时不能以无凭据方式连接。
            None => format!("MCP server {key}: auth is not supported by this runtime yet"),
        };
        raw.get("oauth").map(|_| Failure::Other(message))
    };
    let official = context.loaded.marketplace == crate::official::MARKETPLACE;
    let mut config = json!({"type":kind,"source":{"kind":if official {"builtin"} else {"plugin"}}});
    if let Some(enabled) = raw["enabled"].as_bool() {
        config["enabled"] = enabled.into();
    }
    if raw["timeoutMs"].is_number() {
        config["timeoutMs"] = raw["timeoutMs"].clone();
    }
    if kind == "stdio" {
        let command = required(&raw["command"], "stdio MCP server requires command")?;
        if let Some(conflict) = oauth_conflict() {
            return Err(conflict);
        }
        let root = context.loaded.root.to_string_lossy();
        let data = context.data_path.to_string_lossy();
        let cwd = context.cwd.to_string_lossy();
        let mut env = json!({"CLAUDE_PROJECT_DIR":cwd,"ZCODE_PLUGIN_DATA":data,"ZCODE_PLUGIN_ROOT":root,
            "ZCODE_PROJECT_DIR":cwd,"CLAUDE_PLUGIN_DATA":data,"CLAUDE_PLUGIN_ROOT":root});
        if let Value::Object(extra) = &raw["env"] {
            for (k, v) in extra {
                env[k] = v.clone();
            }
        }
        let mut env = string_record(&env, context)?;
        // 插件身份由解析器权威写入，manifest 的 env 不能覆盖或伪造。
        env.insert("ZCODE_PLUGIN_ID".into(), context.loaded.id.clone().into());
        config["command"] = template(command, context, false)?.into();
        if let Some(args) = raw["args"].as_array() {
            let args = args
                .iter()
                .filter_map(Value::as_str)
                .map(|arg| template(arg, context, false))
                .collect::<Result<Vec<_>, _>>()?;
            config["args"] = args.into();
        }
        if let Some(cwd) = raw["cwd"].as_str() {
            config["cwd"] = template(cwd, context, false)?.into();
        }
        config["env"] = Value::Object(env);
        return Ok(with_auth(config, auth, key, context));
    }
    let url = required(&raw["url"], &format!("{kind} MCP server requires url"))?;
    // 与 Node 相同：headers 先于 url 模板解析，决定两处都出错时报告哪一个。
    if raw["headers"].is_object() {
        config["headers"] = Value::Object(string_record(&raw["headers"], context)?);
    }
    if let Some(conflict) = oauth_conflict() {
        return Err(conflict);
    }
    // 保留头只在官方鉴权路径下拦截；普通 MCP 静态携带 authorization 是既有合法用法。
    let hits = crate::mcp_auth::reserved_hits(&config["headers"]);
    if auth.is_some() && !hits.is_empty() {
        return Err(Failure::Other(format!(
            "MCP server {key}: static headers must not contain reserved header(s): {}",
            hits.join(", ")
        )));
    }
    config["url"] = template(url, context, false)?.into();
    Ok(with_auth(config, auth, key, context))
}

/// The official auth declaration with its host-generated provenance.
fn with_auth(mut config: Value, auth: Option<Value>, key: &str, context: &Context<'_>) -> Value {
    if let Some(auth) = auth {
        config["auth"] = auth;
        config["official"] = crate::mcp_auth::provenance(key, &context.loaded.id);
    }
    config
}

/// Node `resolvePluginMcpServers`: `(plugin:<name>:<key>, config)` of every
/// server that resolves; failures become diagnostics.
pub fn resolve(
    definitions: &Definitions,
    context: &Context<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<(String, Value)> {
    let mut servers = vec![];
    for (key, raw) in definitions {
        match server(key, raw, context) {
            Ok(config) => servers.push((format!("plugin:{}:{key}", context.loaded.name()), config)),
            Err(failure) => {
                let (code, message) = match failure {
                    Failure::Variable(message) => ("plugin_variable_missing", message),
                    Failure::Other(message) => ("plugin_mcp_server_disabled", message),
                };
                diagnostics.push(
                    Diagnostic::new(code, Severity::Error, message)
                        .at(&context.loaded.manifest_path)
                        .plugin(&context.loaded.id),
                );
            }
        }
    }
    servers
}

#[cfg(test)]
#[path = "mcp_tests.rs"]
mod tests;
