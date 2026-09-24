//! Hooks (Node `core/src/hooks`): registrations from config, matching, the
//! stdin contract, output merging, lifecycle payloads and row projection.
//! Processes are started by the tools crate; the run task drives the order.
pub mod decision;
pub mod display;
pub mod input;
pub mod output;
pub mod projection;
pub mod runner;
#[cfg(test)]
mod runner_tests;
mod schema;
#[cfg(test)]
mod tests;

use serde_json::Value;

/// The seven events in Node's canonical order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HookEvent {
    SessionStart,
    UserPromptSubmit,
    PreToolUse,
    PermissionRequest,
    PostToolUse,
    PostToolUseFailure,
    Stop,
}

impl HookEvent {
    pub const ALL: [Self; 7] = [
        Self::SessionStart,
        Self::UserPromptSubmit,
        Self::PreToolUse,
        Self::PermissionRequest,
        Self::PostToolUse,
        Self::PostToolUseFailure,
        Self::Stop,
    ];
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SessionStart => "SessionStart",
            Self::UserPromptSubmit => "UserPromptSubmit",
            Self::PreToolUse => "PreToolUse",
            Self::PermissionRequest => "PermissionRequest",
            Self::PostToolUse => "PostToolUse",
            Self::PostToolUseFailure => "PostToolUseFailure",
            Self::Stop => "Stop",
        }
    }
    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|event| event.as_str() == name)
    }
    /// Events whose output can change a permission decision.
    fn permission(self) -> bool {
        matches!(self, Self::PreToolUse | Self::PermissionRequest)
    }
    /// Events where a block stops the action (Node `shouldPreventContinuation`).
    fn prevents(self) -> bool {
        matches!(
            self,
            Self::PreToolUse | Self::PermissionRequest | Self::UserPromptSubmit
        )
    }
}

/// Node command hook `shell`: unset, `true`, or a shell path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Shell {
    Unset,
    Default,
    Path(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Program {
    Command {
        command: String,
        shell: Shell,
        /// `async: true`: runs in the background, output never merged.
        background: bool,
    },
    Process {
        command: String,
        args: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceKind {
    User,
    Plugin,
    Project,
}

impl SourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Plugin => "plugin",
            Self::Project => "project",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plugin {
    pub id: String,
    pub name: String,
    pub root_path: String,
    pub data_path: String,
    pub source_path: Option<String>,
}

/// One registered hook (Node `HookRegistration` built by `configured-runner.ts`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Registration {
    pub event: HookEvent,
    pub matcher: Option<String>,
    pub program: Program,
    /// `config.<event>.<matcher>.<hook>` / `plugin.<id>.…` / `project.<reviewItemId>`.
    pub source: String,
    pub source_kind: SourceKind,
    pub source_path: Option<String>,
    pub plugin: Option<Plugin>,
    pub status_message: Option<String>,
    pub timeout_ms: u64,
    pub max_output_bytes: usize,
}

impl Registration {
    pub fn background(&self) -> bool {
        matches!(
            self.program,
            Program::Command {
                background: true,
                ..
            }
        )
    }
}

/// JS `Math.max(1, Math.round(x))` on a validated positive number.
fn round_positive(value: f64) -> u64 {
    (value + 0.5).floor().max(1.0) as u64
}

/// Node `resolveWorkspaceHookTimeoutMs`: `timeoutMs`, else a command hook's
/// `timeout` in seconds, else the root default.
pub fn timeout_ms(hook: &Value, default_ms: f64) -> u64 {
    let ms = hook["timeoutMs"].as_f64().unwrap_or_else(|| {
        match (hook["type"].as_str(), hook["timeout"].as_f64()) {
            (Some("command"), Some(seconds)) => seconds * 1000.0,
            _ => default_ms,
        }
    });
    round_positive(ms)
}

/// One declared hook as a program (a validated config entry).
pub fn program(hook: &Value) -> Option<Program> {
    let command = hook["command"].as_str()?.to_owned();
    match hook["type"].as_str()? {
        "command" => Some(Program::Command {
            command,
            shell: match &hook["shell"] {
                Value::Bool(true) => Shell::Default,
                Value::String(path) => Shell::Path(path.clone()),
                _ => Shell::Unset,
            },
            background: hook["async"] == true,
        }),
        "process" => Some(Program::Process {
            command,
            args: hook["args"]
                .as_array()
                .map(|args| {
                    args.iter()
                        .filter_map(|a| a.as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default(),
        }),
        _ => None,
    }
}

/// Node `createConfiguredHookRegistrations` over the merged `hooks` config.
/// Only the user config file can declare executable hooks today, so every
/// entry is a `user` hook from `user_path`. Nothing runs unless `enabled`.
pub fn registrations(hooks: &Value, user_path: Option<&str>) -> Vec<Registration> {
    if hooks["enabled"] != true {
        return vec![];
    }
    let default_ms = hooks["timeoutMs"].as_f64().unwrap_or(60_000.0);
    let max_output_bytes = round_positive(hooks["maxOutputBytes"].as_f64().unwrap_or(32_768.0));
    let mut out = vec![];
    let Some(events) = hooks["events"].as_object() else {
        return out;
    };
    for (name, matchers) in events {
        let Some(event) = HookEvent::parse(name) else {
            continue;
        };
        for (matcher_index, group) in matchers.as_array().into_iter().flatten().enumerate() {
            for (hook_index, hook) in group["hooks"].as_array().into_iter().flatten().enumerate() {
                if hook["enabled"] == false {
                    continue;
                }
                let Some(program) = program(hook) else {
                    continue;
                };
                out.push(Registration {
                    event,
                    matcher: group["matcher"].as_str().map(str::to_owned),
                    program,
                    source: format!("config.{name}.{matcher_index}.{hook_index}"),
                    source_kind: SourceKind::User,
                    source_path: user_path.map(str::to_owned),
                    plugin: None,
                    status_message: hook["statusMessage"].as_str().map(str::to_owned),
                    timeout_ms: timeout_ms(hook, default_ms),
                    max_output_bytes: max_output_bytes as usize,
                });
            }
        }
    }
    out
}

/// Node `hookMatcherToolNamesForTool` plus the runner's own `matchValue`.
pub fn tool_match_values(tool: &str) -> Vec<String> {
    let aliases: &[&str] = match tool {
        "Agent" => &["Task"],
        "Task" => &["Agent"],
        "ApplyPatch" => &["Write", "Edit"],
        _ => &[],
    };
    std::iter::once(tool)
        .chain(aliases.iter().copied())
        .map(str::to_owned)
        .collect()
}

/// Node `matchesAnyHookMatcher`: no matcher or no values match everything.
pub fn matches(matcher: Option<&str>, values: &[String]) -> bool {
    let Some(matcher) = matcher.filter(|m| !m.is_empty()) else {
        return true;
    };
    if values.is_empty() {
        return true;
    }
    let mut seen: Vec<&str> = vec![];
    values.iter().any(|value| {
        if seen.contains(&value.as_str()) {
            return false;
        }
        seen.push(value);
        matches_one(value, matcher)
    })
}

/// Node `matchesHookMatcher`: `*`, `A|B` exact names, else an unanchored JS
/// `RegExp` without flags; an invalid pattern never matches.
fn matches_one(value: &str, matcher: &str) -> bool {
    if matcher == "*" {
        return true;
    }
    if value.is_empty() {
        return false;
    }
    if matcher
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '|')
    {
        return matcher.split('|').any(|name| name == value);
    }
    regress::Regex::new(matcher).is_ok_and(|re| re.find(value).is_some())
}

/// JS `value.length` in UTF-16 units.
pub fn utf16_len(value: &str) -> usize {
    value.encode_utf16().count()
}

/// JS `value.slice(0, units)` on a UTF-16 boundary (a split surrogate pair
/// keeps its high half in JS; Rust drops the whole character).
pub fn utf16_prefix(value: &str, units: usize) -> &str {
    let mut count = 0;
    for (index, c) in value.char_indices() {
        count += c.len_utf16();
        if count > units {
            return &value[..index];
        }
    }
    value
}

/// Node `value.length <= max ? value : value.slice(0, max) + suffix`.
pub fn truncate_utf16(value: &str, max: usize, suffix: &str) -> String {
    if utf16_len(value) <= max {
        value.to_owned()
    } else {
        format!("{}{suffix}", utf16_prefix(value, max))
    }
}
