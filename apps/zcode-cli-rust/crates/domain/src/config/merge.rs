//! Layer merging (Node `config-merger.ts`), MCP server resolution
//! (`resolveEffectiveMcpServers`) and the `ZCODE_*` environment layer
//! (`env-config.adapter.ts`).
use serde_json::{Map, Number, Value, json};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Scope {
    System = 0,
    User = 10,
    Project = 20,
    Env = 40,
    Cli = 50,
}

impl Scope {
    fn mcp_source(self) -> &'static str {
        match self {
            Self::System => "system",
            Self::User => "user",
            Self::Project => "project",
            Self::Env => "env",
            Self::Cli => "cli",
        }
    }
}

fn object(value: Option<&Value>) -> Map<String, Value> {
    value
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default()
}

/// Node `mergePluginOptions`.
fn merge_plugin_options(current: Option<&Value>, next: &Map<String, Value>) -> Value {
    let mut merged = object(current);
    for (plugin, options) in next {
        let mut entry = object(merged.get(plugin));
        entry.extend(object(Some(options)));
        merged.insert(plugin.clone(), Value::Object(entry));
    }
    Value::Object(merged)
}

/// Node `mergeHooksConfig`.
fn merge_hooks(current: Option<&Value>, next: &Map<String, Value>) -> Value {
    let mut events = object(current.and_then(|c| c.get("events")));
    if next.get("enabled") != Some(&Value::Bool(false)) {
        for (event, matchers) in object(next.get("events")) {
            let Value::Array(matchers) = matchers else {
                continue;
            };
            let mut list = events
                .get(&event)
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            list.extend(matchers);
            events.insert(event, Value::Array(list));
        }
    }
    let mut merged = object(current);
    merged.extend(next.clone());
    let enabled = current.and_then(|c| c.get("enabled")) == Some(&Value::Bool(true))
        || next.get("enabled") == Some(&Value::Bool(true));
    merged.insert("enabled".into(), enabled.into());
    merged.insert("events".into(), Value::Object(events));
    Value::Object(merged)
}

/// Node `mergeConfigs`. Faithful to its actual behavior: after `Object.assign`, the
/// per-section spreads re-spread the same object, so a higher layer's section replaces
/// the lower one; only `plugins` and `hooks` merge with the previous layer.
pub fn merge_layers(layers: &[(Scope, &Map<String, Value>)]) -> Map<String, Value> {
    let mut sorted = layers.to_vec();
    sorted.sort_by_key(|(scope, _)| *scope);
    let mut result = Map::new();
    for (scope, input) in sorted {
        let mut config = input.clone();
        if scope == Scope::Project
            && let Some(Value::Object(plugins)) = config.get_mut("plugins")
        {
            plugins.remove("extraKnownMarketplaces");
        }
        let previous_hooks = result.get("hooks").cloned();
        let previous_plugins = result.get("plugins").cloned();
        for (key, value) in &config {
            result.insert(key.clone(), value.clone());
        }
        if let Some(Value::Object(mcp)) = config.get("mcp") {
            let mut merged = mcp.clone();
            merged.insert("servers".into(), Value::Object(object(mcp.get("servers"))));
            result.insert("mcp".into(), Value::Object(merged));
        }
        if let Some(Value::Object(plugins)) = config.get("plugins") {
            let previous = previous_plugins.as_ref();
            let mut merged = object(previous);
            merged.extend(plugins.clone());
            if let Some(Value::Array(dirs)) = plugins.get("dirs") {
                let mut union: Vec<Value> = previous
                    .and_then(|p| p.get("dirs"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                for dir in dirs {
                    if !union.contains(dir) {
                        union.push(dir.clone());
                    }
                }
                let mut unique = vec![];
                for dir in union {
                    if !unique.contains(&dir) {
                        unique.push(dir);
                    }
                }
                merged.insert("dirs".into(), Value::Array(unique));
            }
            for key in ["enabledPlugins", "extraKnownMarketplaces"] {
                if let Some(Value::Object(next)) = plugins.get(key) {
                    let mut entry = object(previous.and_then(|p| p.get(key)));
                    entry.extend(next.clone());
                    merged.insert(key.into(), Value::Object(entry));
                }
            }
            if let Some(Value::Object(options)) = plugins.get("options") {
                merged.insert(
                    "options".into(),
                    merge_plugin_options(previous.and_then(|p| p.get("options")), options),
                );
            }
            result.insert("plugins".into(), Value::Object(merged));
        }
        if let Some(Value::Object(hooks)) = config.get("hooks") {
            result.insert("hooks".into(), merge_hooks(previous_hooks.as_ref(), hooks));
        }
    }
    result
}

/// Node `resolveEffectiveMcpServers`: user config shadows project config for MCP only.
pub fn resolve_mcp(
    layers: &[(Scope, &Map<String, Value>)],
) -> (Map<String, Value>, Map<String, Value>) {
    let order = [
        Scope::System,
        Scope::Project,
        Scope::User,
        Scope::Env,
        Scope::Cli,
    ];
    let mut servers = Map::new();
    let mut sources = Map::new();
    for scope in order {
        for (_, patch) in layers.iter().filter(|(s, _)| *s == scope) {
            for (name, server) in object(patch.get("mcp").and_then(|m| m.get("servers"))) {
                servers.insert(name.clone(), server);
                sources.insert(name, scope.mcp_source().into());
            }
        }
    }
    (servers, sources)
}

/// JavaScript `Number(value)`; `None` for NaN.
fn js_number(value: &str) -> Option<f64> {
    let s = value.trim();
    if s.is_empty() {
        return Some(0.0);
    }
    match s {
        "Infinity" | "+Infinity" => return Some(f64::INFINITY),
        "-Infinity" => return Some(f64::NEG_INFINITY),
        _ => {}
    }
    for (prefix, radix) in [
        ("0x", 16),
        ("0X", 16),
        ("0o", 8),
        ("0O", 8),
        ("0b", 2),
        ("0B", 2),
    ] {
        if let Some(digits) = s.strip_prefix(prefix) {
            return u64::from_str_radix(digits, radix).ok().map(|n| n as f64);
        }
    }
    let decimal = s
        .strip_prefix(['+', '-'])
        .unwrap_or(s)
        .chars()
        .all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-'));
    if !decimal || s.contains(|c: char| c.is_ascii_alphabetic() && c != 'e' && c != 'E') {
        return None;
    }
    s.parse::<f64>().ok()
}

fn number_value(n: f64) -> Value {
    if n.fract() == 0.0 && n.abs() < 9.007_199_254_740_992e15 {
        json!(n as i64)
    } else {
        Number::from_f64(if n.is_finite() {
            n
        } else {
            f64::MAX.copysign(n)
        })
        .map_or(Value::Null, Value::Number)
    }
}

/// Node `normalizeNumber`: NaN becomes 0 (kept as Node behaves, D13).
fn env_number(value: &str) -> Value {
    number_value(js_number(value).unwrap_or(0.0))
}

fn set(config: &mut Map<String, Value>, section: &str, key: &str, value: Value) {
    let entry = config
        .entry(section.to_owned())
        .or_insert_with(|| json!({}));
    entry[key] = value;
}

/// Node `parseEnvConfig`, iterating variables in environment order.
pub fn env_patch<'a>(env: impl IntoIterator<Item = (&'a str, &'a str)>) -> Map<String, Value> {
    let mut config = Map::new();
    for (key, value) in env {
        let Some(key) = key.strip_prefix("ZCODE_") else {
            continue;
        };
        match key {
            "STORAGE_DIR" => set(&mut config, "storage", "dir", value.into()),
            "SESSION_DB_PATH" | "SESSION_DB" => {
                set(&mut config, "storage", "sessionDbPath", value.into())
            }
            "HTTP_PROXY" => set(&mut config, "network", "httpProxy", value.into()),
            "NO_PROXY" => set(&mut config, "network", "noProxy", value.into()),
            "AGENT_CA_CERT" => set(&mut config, "network", "caCertFile", value.into()),
            "HTTP_TIMEOUT" | "TIMEOUT" => set(&mut config, "network", "timeout", env_number(value)),
            "LOG_FORMAT" => {
                let format = value.to_lowercase();
                let format = if format == "json" { "json" } else { "text" };
                set(&mut config, "logging", "format", format.into());
            }
            "MAX_TOOL_CONCURRENCY" => set(
                &mut config,
                "toolConcurrency",
                "maxConcurrency",
                env_number(value),
            ),
            _ => {}
        }
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_number_follows_javascript() {
        assert_eq!(js_number(""), Some(0.0));
        assert_eq!(js_number(" 12 "), Some(12.0));
        assert_eq!(js_number("0x10"), Some(16.0));
        assert_eq!(js_number("1e3"), Some(1000.0));
        assert_eq!(js_number("abc"), None);
        assert_eq!(js_number("inf"), None);
        assert_eq!(env_number("abc"), json!(0));
    }
}
