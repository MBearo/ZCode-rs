//! zcode configuration, ported from Node `adapters/src/config` (pure logic).
//!
//! Semantics follow the Node implementation as it behaves today, including its
//! quirks: after `Object.assign`, most sections of a higher-priority layer
//! replace the lower layer's section entirely (only `plugins` and `hooks` are
//! merged), and missing fields fall back per field in [`effective`]. Parity is
//! checked against fixtures generated from the TS functions.
mod file;
mod merge;

pub use file::{LoadedFile, parse_file, project_file};
pub use merge::{Scope, env_patch, merge_layers, resolve_mcp};

use serde::Serialize;
use serde_json::{Map, Value, json};

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Diagnostic {
    pub code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub severity: &'static str,
}

/// Node `DefaultRuntimeConfig`.
pub fn defaults() -> Map<String, Value> {
    let Value::Object(map) = json!({
        "modelStream": {"idleTimeoutMs": 600_000},
        "permission": {"mode":"build","allowedTools":[],"disallowedTools":[],"autoApproveHighRisk":false,"allowMediumRiskInAuto":false},
        "storage": {"dir":"~/.zcode","sessionDbPath":"~/.zcode/cli/db/db.sqlite"},
        "network": {"timeout":180_000},
        "features": {"compact":true,"rewind":true,"subagent":true,"memory":true,"skill":true,"mcp":true},
        "memory": {"use":true},
        "mcp": {"servers":{}},
        "plugins": {"dirs":[],"enabled":true,"enabledPlugins":{},"extraKnownMarketplaces":{},"options":{},"suppressedBuiltins":[]},
        "skills": {"enabled":true,"includeInstructions":true,"metadataBudget":20_000,"roots":[]},
        "skillOverrides": {},
        "commandOverrides": {},
        "logging": {"level":"info","format":"text"},
        "toolConcurrency": {"maxConcurrency":10},
        "modelAnomalyGuard": {"maxBudgetWarningsPerTurn":3,"repeatedToolCallWarningThreshold":3},
        "hooks": {"enabled":false,"events":{},"maxOutputBytes":32768,"timeoutMs":60000},
        "ui": {"locale":"en-US","theme":"auto"},
    }) else {
        unreachable!()
    };
    map
}

fn field(merged: &Map<String, Value>, section: &str, key: &str) -> Option<Value> {
    merged
        .get(section)
        .and_then(|s| s.get(key))
        .filter(|v| !v.is_null())
        .cloned()
}

fn or_default(merged: &Map<String, Value>, section: &str, key: &str) -> Value {
    field(merged, section, key)
        .or_else(|| defaults()[section].get(key).cloned())
        .unwrap_or(Value::Null)
}

fn whole_or_default(merged: &Map<String, Value>, key: &str) -> Value {
    merged
        .get(key)
        .filter(|v| !v.is_null())
        .cloned()
        .unwrap_or_else(|| defaults()[key].clone())
}

/// Node `ConfigPortImpl.getAll()`: the effective config with per-field defaults.
/// Unknown top-level keys are not part of the effective config.
pub fn effective(merged: &Map<String, Value>) -> Value {
    let mut network = Map::new();
    for key in ["httpProxy", "noProxy", "caCertFile"] {
        if let Some(value) = field(merged, "network", key) {
            network.insert(key.into(), value);
        }
    }
    network.insert("timeout".into(), or_default(merged, "network", "timeout"));
    let section = |name: &str, keys: &[&str]| {
        Value::Object(
            keys.iter()
                .map(|key| ((*key).to_owned(), or_default(merged, name, key)))
                .collect(),
        )
    };
    json!({
        "modelStream": section("modelStream", &["idleTimeoutMs"]),
        "permission": section("permission", &["mode","allowedTools","disallowedTools","autoApproveHighRisk","allowMediumRiskInAuto"]),
        "storage": section("storage", &["dir","sessionDbPath"]),
        "network": network,
        "features": section("features", &["compact","rewind","subagent","memory","skill","mcp"]),
        "memory": section("memory", &["use"]),
        "mcp": section("mcp", &["servers"]),
        "plugins": section("plugins", &["dirs","enabled","enabledPlugins","extraKnownMarketplaces","options","suppressedBuiltins"]),
        "skills": section("skills", &["enabled","includeInstructions","metadataBudget","roots"]),
        "skillOverrides": whole_or_default(merged, "skillOverrides"),
        "commandOverrides": whole_or_default(merged, "commandOverrides"),
        "logging": section("logging", &["level","format"]),
        "toolConcurrency": section("toolConcurrency", &["maxConcurrency"]),
        "modelAnomalyGuard": whole_or_default(merged, "modelAnomalyGuard"),
        "hooks": whole_or_default(merged, "hooks"),
        "ui": section("ui", &["locale","theme"]),
    })
}

/// A loaded project config file that declares hooks (Node `hookCandidate`).
#[derive(Clone, Debug, PartialEq)]
pub struct HookCandidate {
    pub path: String,
    /// Position among every discovered project config file, loaded or not.
    pub discovery_order: usize,
    pub hooks: Value,
}

/// Immutable result of loading every configuration layer for one workspace.
#[derive(Clone, Debug, Default)]
pub struct ConfigSnapshot {
    /// Node `getAll()` shape with defaults applied.
    pub config: Value,
    /// MCP server name → source (`system|project|user|env|cli`).
    pub mcp_sources: Map<String, Value>,
    /// Project hook declarations kept outside the executable config (trust candidates).
    pub project_hook_candidates: Vec<HookCandidate>,
    /// The user config file's `hooks` root (for the workspace hook runtime root).
    pub user_hooks: Value,
    pub user_path: String,
    pub project_paths: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

impl ConfigSnapshot {
    pub fn get(&self, section: &str, key: &str) -> &Value {
        &self.config[section][key]
    }
    pub fn str(&self, section: &str, key: &str) -> Option<&str> {
        self.get(section, key).as_str()
    }
}

#[cfg(test)]
mod tests;
