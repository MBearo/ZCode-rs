//! `plugins/list` (Node `listPlugins`, `toPluginInfo`,
//! `createMissingConfiguredPluginInfos`). Spec rust-m10-plugins §3.6.
use crate::discovery::{Outcome, Plugin};
use crate::loaded::Source;
use crate::manifest::Diagnostic;
use serde_json::{Map, Value, json};
use std::path::Path;

/// The `plugins` sections of the user file and the merged project files
/// (Node `PluginConfigSources`).
pub struct Sources<'a> {
    pub user: &'a Value,
    pub workspace: &'a Value,
    pub cwd: &'a Path,
}

impl Sources<'_> {
    /// The scope that last declared `key` under `section` (workspace wins).
    fn scope(&self, section: &str, id: &str) -> Option<&'static str> {
        if self.workspace[section].get(id).is_some() {
            Some("workspace")
        } else if self.user[section].get(id).is_some() {
            Some("user")
        } else {
            None
        }
    }

    fn option_sources(&self, id: &str) -> Map<String, Value> {
        let mut sources = Map::new();
        for (scope, layer) in [("user", self.user), ("workspace", self.workspace)] {
            for key in layer["options"][id]
                .as_object()
                .into_iter()
                .flat_map(|o| o.keys())
            {
                sources.insert(key.clone(), scope.into());
            }
        }
        sources
    }

    /// Node `resolveInlinePluginRootSource`: workspace declarations first.
    fn root_source(&self, root: &Path) -> Option<&'static str> {
        let key = comparable(root);
        [("workspace", self.workspace), ("user", self.user)]
            .into_iter()
            .find(|(_, layer)| {
                crate::fsx::path_list(&layer["dirs"])
                    .iter()
                    .any(|dir| comparable(&crate::fsx::resolve(self.cwd, dir)) == key)
            })
            .map(|(scope, _)| scope)
    }
}

/// Windows paths compare case-insensitively with either separator.
fn comparable(path: &Path) -> String {
    let text = crate::fsx::normalize(path).to_string_lossy().into_owned();
    if cfg!(windows) {
        text.replace('\\', "/").to_lowercase()
    } else {
        text
    }
}

fn text(value: &Value) -> Option<Value> {
    value.as_str().map(|s| Value::String(s.to_owned()))
}

/// Node `toPluginInfo`.
pub fn info(plugin: &Plugin, sources: Option<&Sources<'_>>) -> Value {
    let loaded = &plugin.loaded;
    let manifest = &loaded.manifest;
    let user_config = &manifest["userConfig"];
    let options: Map<String, Value> = plugin
        .configured_options
        .iter()
        .filter(|(key, _)| user_config[key.as_str()]["sensitive"] != true)
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    let components: Vec<Value> = plugin.components.iter().map(|g| g.protocol()).collect();
    let mut info = json!({"id":loaded.id,"name":loaded.name(),"enabled":plugin.enabled,
        "source":loaded.source.as_str(),"marketplace":loaded.marketplace,
        "skillCount":plugin.skill_count,"skillRootCount":plugin.skill_root_count,
        "commandRootCount":plugin.command_root_count,"components":components,
        "declaredMcpServerNames":plugin.declared_mcp,"mcpServerNames":plugin.mcp_server_names,
        "hookDetails":plugin.hook_details,"rootPath":loaded.root});
    let mut put = |key: &str, value: Option<Value>| {
        if let Some(value) = value {
            info[key] = value;
        }
    };
    put("description", text(&manifest["description"]));
    put("version", text(&manifest["version"]));
    if let Some((name, url)) = crate::manifest::author(&manifest["author"]) {
        put("author", name.map(Value::String));
        put("authorUrl", url.map(Value::String));
    }
    put(
        "homepage",
        crate::manifest::text(&manifest["homepage"]).map(Value::String),
    );
    let host = crate::official::host_mcp_server_names(&loaded.id);
    put(
        "hostMcpServerNames",
        (!host.is_empty()).then(|| json!(host)),
    );
    put(
        "userConfig",
        user_config.is_object().then(|| user_config.clone()),
    );
    put(
        "configuredOptions",
        (!options.is_empty()).then_some(Value::Object(options)),
    );
    if let Some(sources) = sources {
        if loaded.source == Source::Inline {
            put(
                "rootSource",
                sources.root_source(&loaded.root).map(Value::from),
            );
        }
        put(
            "enabledSource",
            sources.scope("enabledPlugins", &loaded.id).map(Value::from),
        );
        let option_sources = sources.option_sources(&loaded.id);
        put(
            "optionSources",
            (!option_sources.is_empty()).then_some(Value::Object(option_sources)),
        );
    }
    info
}

/// Node `createMissingConfiguredPluginInfos`.
fn missing(config: &Value, discovered: &[&str], sources: &Sources<'_>) -> Vec<Value> {
    let plugins = &config["plugins"];
    let mut ids: Vec<&String> = vec![];
    for section in ["enabledPlugins", "options"] {
        for id in plugins[section]
            .as_object()
            .into_iter()
            .flat_map(|o| o.keys())
        {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
    }
    ids.into_iter()
        .filter(|id| !discovered.contains(&id.as_str()))
        .filter_map(|id| {
            let (name, marketplace) = crate::records::split_id(id)?;
            let mut info = json!({"id":id,"name":name,
                "enabled":plugins["enabledPlugins"][id].as_bool().unwrap_or(false),
                "source":"missing","marketplace":marketplace,"skillCount":0,"skillRootCount":0,
                "commandRootCount":0,"components":[],"declaredMcpServerNames":[],"mcpServerNames":[],
                "rootPath":"","packageStatus":"missing"});
            if let Some(scope) = sources.scope("enabledPlugins", id) {
                info["enabledSource"] = scope.into();
            }
            let option_sources = sources.option_sources(id);
            if !option_sources.is_empty() {
                info["optionSources"] = Value::Object(option_sources);
            }
            Some(info)
        })
        .collect()
}

/// `ZCodePluginsListResult` for a discovery over `config`.
pub fn list(outcome: &Outcome, config: &Value, sources: &Sources<'_>) -> Value {
    let mut plugins: Vec<Value> = outcome
        .plugins
        .iter()
        .map(|p| info(p, Some(sources)))
        .collect();
    let discovered: Vec<&str> = outcome
        .plugins
        .iter()
        .map(|p| p.loaded.id.as_str())
        .collect();
    plugins.extend(missing(config, &discovered, sources));
    let diagnostics: Vec<Value> = outcome
        .diagnostics
        .iter()
        .map(Diagnostic::protocol)
        .collect();
    json!({ "plugins": plugins, "diagnostics": diagnostics })
}
