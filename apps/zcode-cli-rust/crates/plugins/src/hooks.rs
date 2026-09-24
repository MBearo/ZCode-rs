//! Plugin hook sources, matcher validation and details (Node
//! `hook-sources.ts` and the hook inspection of `plugins/index.ts`).
use crate::loaded::Loaded;
use crate::manifest::{Diagnostic, Severity};
use serde_json::{Map, Value, json};
use std::path::{Path, PathBuf};
use zcode_cli_domain::hooks::HookEvent;

pub struct Source {
    pub raw: Value,
    pub path: PathBuf,
    /// A hooks file: the events are under `hooks`.
    pub wrapper: bool,
}

/// Validated matchers per event plus the display details.
#[derive(Default)]
pub struct Inspection {
    pub details: Vec<Value>,
    /// `(event, {matcher?, hooks: [hook + plugin context]})` in declaration order.
    pub events: Vec<(HookEvent, Value)>,
}

async fn load(path: &Path, loaded: &Loaded, diagnostics: &mut Vec<Diagnostic>) -> Option<Source> {
    let failure = |message: String| {
        Diagnostic::new("plugin_hook_read_failed", Severity::Error, message)
            .at(path)
            .plugin(&loaded.id)
    };
    match crate::fsx::read_json(path).await {
        Ok(Ok(raw)) => Some(Source {
            raw,
            path: path.to_owned(),
            wrapper: true,
        }),
        Ok(Err(message)) => {
            diagnostics.push(failure(message));
            None
        }
        Err(error) => {
            diagnostics.push(failure(error.to_string()));
            None
        }
    }
}

/// Node `listPluginHookSources`: `hooks/hooks.json`, then `manifest.hooks`
/// (paths inside the root, deduplicated by real path, or inline objects).
pub async fn sources(loaded: &Loaded, diagnostics: &mut Vec<Diagnostic>) -> Vec<Source> {
    let mut sources = vec![];
    let mut seen = vec![];
    let standard = loaded.root.join("hooks").join("hooks.json");
    if crate::fsx::is_file(&standard).await
        && let Some(source) = load(&standard, loaded, diagnostics).await
    {
        sources.push(source);
        seen.push(crate::fsx::realpath_or_self(&standard).await);
    }
    let specs = match &loaded.manifest["hooks"] {
        Value::Null => return sources,
        Value::Array(items) => items.clone(),
        other => vec![other.clone()],
    };
    for spec in specs {
        let Value::String(raw) = &spec else {
            sources.push(Source {
                raw: spec,
                path: loaded.manifest_path.clone(),
                wrapper: false,
            });
            continue;
        };
        let Some(path) = crate::fsx::resolve_inside(&loaded.root, raw) else {
            diagnostics.push(
                Diagnostic::new(
                    "plugin_component_path_invalid",
                    Severity::Error,
                    format!("Plugin hooks path escapes plugin root: {raw}"),
                )
                .at(&loaded.manifest_path)
                .plugin(&loaded.id),
            );
            continue;
        };
        if !crate::fsx::is_file(&path).await {
            diagnostics.push(
                Diagnostic::new(
                    "plugin_hook_read_failed",
                    Severity::Error,
                    format!("Plugin hooks file not found: {raw}"),
                )
                .at(&path)
                .plugin(&loaded.id),
            );
            continue;
        }
        let real = crate::fsx::realpath_or_self(&path).await;
        if seen.contains(&real) {
            diagnostics.push(
                Diagnostic::new(
                    "plugin_hook_invalid",
                    Severity::Warning,
                    format!("Duplicate plugin hooks file ignored: {raw}"),
                )
                .at(&path)
                .plugin(&loaded.id),
            );
            continue;
        }
        if let Some(source) = load(&path, loaded, diagnostics).await {
            sources.push(source);
            seen.push(real);
        }
    }
    sources
}

/// The events object of a source, or the Node diagnostic when it has none.
fn events_of<'a>(
    source: &'a Source,
    loaded: &Loaded,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<&'a Map<String, Value>> {
    let root = if source.wrapper {
        &source.raw["hooks"]
    } else {
        &source.raw
    };
    if let Value::Object(events) = root {
        return Some(events);
    }
    let message = if source.wrapper {
        "Plugin hooks file must contain a hooks object"
    } else {
        "Plugin manifest hooks entry must be an object, a path, or an array"
    };
    diagnostics.push(
        Diagnostic::new("plugin_hook_invalid", Severity::Error, message)
            .at(&source.path)
            .plugin(&loaded.id),
    );
    None
}

fn unsupported(name: &str, source: &Source, loaded: &Loaded) -> Diagnostic {
    Diagnostic::new(
        "plugin_hook_unsupported_event",
        Severity::Warning,
        format!("Plugin hook event is not supported by this ZCode runtime: {name}"),
    )
    .at(&source.path)
    .plugin(&loaded.id)
}

/// Node `listPluginHookEventNames`: supported event names, deduplicated.
pub fn event_names(
    sources: &[Source],
    loaded: &Loaded,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let mut names: Vec<String> = vec![];
    for source in sources {
        let Some(events) = events_of(source, loaded, diagnostics) else {
            continue;
        };
        for name in events.keys() {
            if HookEvent::parse(name).is_none() {
                diagnostics.push(unsupported(name, source, loaded));
            } else if !names.contains(name) {
                names.push(name.clone());
            }
        }
    }
    names
}

/// Node `parsePluginHookEvents` over every source.
pub fn inspect(
    sources: &[Source],
    loaded: &Loaded,
    data_path: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> Inspection {
    let mut inspection = Inspection::default();
    for source in sources {
        let Some(events) = events_of(source, loaded, diagnostics) else {
            continue;
        };
        let plugin = json!({"dataPath":data_path,"id":loaded.id,"name":loaded.manifest["name"],
            "rootPath":loaded.root,"sourcePath":source.path});
        for (name, matchers) in events {
            let Some(event) = HookEvent::parse(name) else {
                diagnostics.push(unsupported(name, source, loaded));
                continue;
            };
            let Some(matchers) = matchers.as_array() else {
                diagnostics.push(
                    Diagnostic::new(
                        "plugin_hook_invalid",
                        Severity::Error,
                        format!("Plugin hook event must be an array: {name}"),
                    )
                    .at(&source.path)
                    .plugin(&loaded.id),
                );
                continue;
            };
            for matcher in matchers {
                let mut valid = match crate::hook_schema::matcher(matcher) {
                    Ok(valid) => valid,
                    Err(issues) => {
                        diagnostics.push(
                            Diagnostic::new(
                                "plugin_hook_invalid",
                                Severity::Error,
                                format!("Invalid plugin hook matcher for {name}: {issues}"),
                            )
                            .at(&source.path)
                            .plugin(&loaded.id),
                        );
                        continue;
                    }
                };
                for hook in valid["hooks"].as_array().into_iter().flatten() {
                    inspection
                        .details
                        .push(detail(event, hook, &valid["matcher"], &source.path));
                }
                for hook in valid["hooks"].as_array_mut().into_iter().flatten() {
                    hook["plugin"] = plugin.clone();
                }
                inspection.events.push((event, valid));
            }
        }
    }
    inspection
}

/// Node `toPluginHookDetail` (hooks are always runnable, see `canRunPluginHooks`).
fn detail(event: HookEvent, hook: &Value, matcher: &Value, source: &Path) -> Value {
    let mut detail = json!({"command":hook["command"],"event":event.as_str(),"runnable":true,
        "sourcePath":source,"type":hook["type"]});
    if !matcher.is_null() {
        detail["matcher"] = matcher.clone();
    }
    let keys: &[&str] = if hook["type"] == "process" {
        &["statusMessage", "timeoutMs", "args"]
    } else {
        &["statusMessage", "timeoutMs", "async", "shell", "timeout"]
    };
    for key in keys {
        if let Some(value) = hook.get(*key) {
            detail[*key] = value.clone();
        }
    }
    detail
}
