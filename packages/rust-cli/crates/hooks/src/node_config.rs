//! Node CLI 配置文件里的 `hooks` 段（docs/specs/rust-cli/hooks.md §2.2、config.md §4）。
//!
//! 外形是 Node 的 `HooksRuntimeConfigPatch`（`apps/zcode-cli/packages/contracts/src/hooks/index.ts`、
//! `adapters/src/config/schema.ts`）：`{enabled, timeoutMs, maxOutputBytes, events: {<事件>: [{matcher,
//! hooks: [{type: "command" | "process", …}]}]}}`。事件名接受 hooks 设置的全部 33 个（Node 只认 7 个）。
//! 这里只解析与转换，不做 IO；坏项跳过并给出警告（D1）。项目级的信任 digest 见 `workspace_digest`。

use rust_cli_kernel_api::hooks::HookEventName;
use serde_json::{Map, Value};

use crate::config::{CommandHook, HookCommand, HookMatcher, HookShell, HooksSettings};

/// Node 的默认值（`DefaultHooksRuntimeConfig`）。
pub const DEFAULT_TIMEOUT_MS: f64 = 60_000.0;
pub const DEFAULT_MAX_OUTPUT_BYTES: f64 = 32_768.0;

/// Node 的 7 个事件，按 `WORKSPACE_HOOK_EVENT_NAMES` 的顺序（digest 按这个顺序遍历）：只定义在 kernel-api（F58：
/// 以前这里与 platform 的信任记录各写一份）。
pub use rust_cli_kernel_api::hooks::NODE_EVENTS;

/// command hook 的 `shell`（Node `true | string`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NodeShell {
    Unset,
    True,
    Named(String),
}

#[derive(Debug, Clone, PartialEq)]
pub enum NodeHookKind {
    /// `type: "command"`：经 shell 执行。`timeout` 是秒。
    Command {
        shell: NodeShell,
        background: bool,
        timeout: Option<f64>,
    },
    /// `type: "process"`：`command` + `args`，不经 shell（Node `mode: "argv"`）。
    Process { args: Vec<String> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeHook {
    pub command: String,
    pub kind: NodeHookKind,
    pub enabled: Option<bool>,
    pub timeout_ms: Option<f64>,
    pub status_message: Option<String>,
    /// `onError: "deny"`：PreToolUse 里这个 hook 出错时拦下工具（hooks.md §4.1 末段）；`"allow"` 与缺省同义。
    pub deny_on_error: bool,
}

impl NodeHook {
    /// Node `resolveWorkspaceHookTimeoutMs`：`timeoutMs ?? timeout × 1000（只对 command）?? 默认`，
    /// 四舍五入，至少 1。
    pub fn resolved_timeout_ms(&self, default_timeout_ms: f64) -> f64 {
        let timeout = self.timeout_ms.unwrap_or(match &self.kind {
            NodeHookKind::Command {
                timeout: Some(seconds),
                ..
            } => seconds * rust_cli_base::units::MS_PER_SECOND_F64,
            NodeHookKind::Command { timeout: None, .. } | NodeHookKind::Process { .. } => {
                default_timeout_ms
            }
        });
        timeout.round().max(1.0)
    }

    /// 结果与提示里显示的命令（Node `formatCommand` / `displayCommandAtGrant`）。
    pub fn display(&self) -> String {
        match &self.kind {
            NodeHookKind::Process { args } if !args.is_empty() => {
                format!("{} {}", self.command, args.join(" "))
            }
            NodeHookKind::Process { .. } | NodeHookKind::Command { .. } => self.command.clone(),
        }
    }

    /// 转成 hooks crate 的 command hook；超时总是显式给出（秒）。
    pub fn to_command(&self, default_timeout_ms: f64, warnings: &mut Vec<String>) -> HookCommand {
        let timeout = Some(
            self.resolved_timeout_ms(default_timeout_ms) / rust_cli_base::units::MS_PER_SECOND_F64,
        );
        let hook = match &self.kind {
            NodeHookKind::Command {
                shell, background, ..
            } => CommandHook {
                command: self.command.clone(),
                args: None,
                shell: hook_shell(shell, warnings),
                timeout,
                condition: None,
                background: *background,
                plugin_root: None,
                deny_on_error: self.deny_on_error,
            },
            NodeHookKind::Process { args } => CommandHook {
                command: self.command.clone(),
                args: Some(args.clone()),
                shell: HookShell::Bash,
                timeout,
                condition: None,
                background: false,
                plugin_root: None,
                deny_on_error: self.deny_on_error,
            },
        };
        HookCommand::Command(hook)
    }
}

fn hook_shell(shell: &NodeShell, warnings: &mut Vec<String>) -> HookShell {
    match shell {
        NodeShell::Unset | NodeShell::True => HookShell::Bash,
        NodeShell::Named(name) => match name.as_str() {
            "bash" => HookShell::Bash,
            "powershell" | "pwsh" => HookShell::PowerShell,
            other => {
                warnings.push(format!("hook shell `{other}` is not supported; using bash"));
                HookShell::Bash
            }
        },
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct NodeMatcher {
    pub matcher: Option<String>,
    pub hooks: Vec<NodeHook>,
}

/// 一份配置文件的 `hooks` 段。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct NodeHooks {
    pub enabled: Option<bool>,
    pub timeout_ms: Option<f64>,
    pub max_output_bytes: Option<f64>,
    pub events: Vec<(HookEventName, Vec<NodeMatcher>)>,
}

fn positive(object: &Map<String, Value>, key: &str) -> Result<Option<f64>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(value) => match value.as_f64() {
            Some(number) if number > 0.0 && number.is_finite() => Ok(Some(number)),
            _ => Err(format!("`{key}` must be a positive number")),
        },
    }
}

fn boolean(object: &Map<String, Value>, key: &str) -> Result<Option<bool>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::Bool(value)) => Ok(Some(*value)),
        Some(_) => Err(format!("`{key}` must be a boolean")),
    }
}

fn non_empty(object: &Map<String, Value>, key: &str) -> Result<Option<String>, String> {
    match object.get(key) {
        None => Ok(None),
        Some(Value::String(text)) if !text.is_empty() => Ok(Some(text.clone())),
        Some(_) => Err(format!("`{key}` must be a non-empty string")),
    }
}

fn parse_hook(value: &Value) -> Result<NodeHook, String> {
    let object = value.as_object().ok_or("hook is not an object")?;
    let command = non_empty(object, "command")?.ok_or("missing `command`")?;
    let kind = match object.get("type").and_then(Value::as_str) {
        Some("command") => NodeHookKind::Command {
            shell: match object.get("shell") {
                None => NodeShell::Unset,
                Some(Value::Bool(true)) => NodeShell::True,
                Some(Value::String(name)) if !name.is_empty() => NodeShell::Named(name.clone()),
                Some(_) => return Err("`shell` must be true or a shell name".to_owned()),
            },
            background: boolean(object, "async")? == Some(true),
            timeout: positive(object, "timeout")?,
        },
        Some("process") => NodeHookKind::Process {
            args: match object.get("args") {
                None => Vec::new(),
                Some(Value::Array(items)) => items
                    .iter()
                    .map(|item| item.as_str().map(str::to_owned))
                    .collect::<Option<_>>()
                    .ok_or("`args` must be an array of strings")?,
                Some(_) => return Err("`args` must be an array of strings".to_owned()),
            },
        },
        Some(other) => return Err(format!("hook type `{other}` is not supported")),
        None => return Err("hook has no type".to_owned()),
    };
    // 修复线 F68（审计 C1）：以前不解析 `onError`，守卫 hook 出错时工具照常执行，Node 与 App 会拦下；带 `deny` 的项目
    // 声明 digest 也与 Node 不同，授权互不认。
    let deny_on_error = match object.get("onError") {
        None => false,
        Some(Value::String(value)) if value == "deny" => true,
        Some(Value::String(value)) if value == "allow" => false,
        Some(_) => return Err("`onError` must be \"allow\" or \"deny\"".to_owned()),
    };
    Ok(NodeHook {
        command,
        kind,
        enabled: boolean(object, "enabled")?,
        timeout_ms: positive(object, "timeoutMs")?,
        status_message: non_empty(object, "statusMessage")?,
        deny_on_error,
    })
}

fn parse_matchers(event: &str, value: &Value, warnings: &mut Vec<String>) -> Vec<NodeMatcher> {
    let Some(items) = value.as_array() else {
        warnings.push(format!("hooks.events.{event}: not an array"));
        return Vec::new();
    };
    let mut matchers = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let at = format!("hooks.events.{event}[{index}]");
        let Some(object) = item.as_object() else {
            warnings.push(format!("{at}: not an object"));
            continue;
        };
        let matcher = match non_empty(object, "matcher") {
            Ok(matcher) => matcher,
            Err(error) => {
                warnings.push(format!("{at}: {error}"));
                continue;
            }
        };
        let Some(list) = object.get("hooks").and_then(Value::as_array) else {
            warnings.push(format!("{at}.hooks: not an array"));
            continue;
        };
        let mut hooks = Vec::new();
        for (position, hook) in list.iter().enumerate() {
            match parse_hook(hook) {
                Ok(hook) => hooks.push(hook),
                Err(error) => warnings.push(format!("{at}.hooks[{position}]: {error}")),
            }
        }
        if !hooks.is_empty() {
            matchers.push(NodeMatcher { matcher, hooks });
        }
    }
    matchers
}

impl NodeHooks {
    /// 解析 `hooks` 段的值，返回结果与警告。
    pub fn from_json(value: &Value) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut parsed = Self::default();
        let Some(object) = value.as_object() else {
            warnings.push("hooks: not an object".to_owned());
            return (parsed, warnings);
        };
        let root = |key: &str, warnings: &mut Vec<String>| {
            positive(object, key).unwrap_or_else(|error| {
                warnings.push(format!("hooks: {error}"));
                None
            })
        };
        parsed.timeout_ms = root("timeoutMs", &mut warnings);
        parsed.max_output_bytes = root("maxOutputBytes", &mut warnings);
        parsed.enabled = boolean(object, "enabled").unwrap_or_else(|error| {
            warnings.push(format!("hooks: {error}"));
            None
        });
        match object.get("events") {
            None => {}
            Some(Value::Object(events)) => {
                for (name, value) in events {
                    let Some(event) = HookEventName::parse(name) else {
                        warnings.push(format!("hooks.events.{name}: unknown hook event"));
                        continue;
                    };
                    let matchers = parse_matchers(name, value, &mut warnings);
                    if !matchers.is_empty() {
                        parsed.events.push((event, matchers));
                    }
                }
            }
            Some(_) => warnings.push("hooks.events: not an object".to_owned()),
        }
        (parsed, warnings)
    }

    /// 某个事件的 matcher（同一事件只出现一次：JSON 对象的键唯一）。
    pub fn matchers(&self, event: HookEventName) -> &[NodeMatcher] {
        self.events
            .iter()
            .find(|(name, _)| *name == event)
            .map_or(&[], |(_, matchers)| matchers.as_slice())
    }

    /// 用户级 hooks 的快照来源（Node `mergeHooksConfig` + `createConfiguredHookRunner`）：只有用户级
    /// 写了 `enabled: true` 才运行（Node 默认 `false`）；`enabled: false` 的声明跳过；超时按 Node 解析，
    /// 默认取本段的 `timeoutMs`。
    pub fn user_settings(&self) -> (HooksSettings, Vec<String>) {
        let mut warnings = Vec::new();
        let mut settings = HooksSettings::default();
        if self.enabled != Some(true) {
            return (settings, warnings);
        }
        let default_timeout = self.timeout_ms.unwrap_or(DEFAULT_TIMEOUT_MS);
        for (event, matchers) in &self.events {
            let converted: Vec<HookMatcher> = matchers
                .iter()
                .map(|matcher| HookMatcher {
                    matcher: matcher.matcher.clone(),
                    hooks: matcher
                        .hooks
                        .iter()
                        .filter(|hook| hook.enabled != Some(false))
                        .map(|hook| hook.to_command(default_timeout, &mut warnings))
                        .collect(),
                })
                .filter(|matcher| !matcher.hooks.is_empty())
                .collect();
            if !converted.is_empty() {
                settings.hooks.push((*event, converted));
            }
        }
        (settings, warnings)
    }
}

#[cfg(test)]
#[path = "node_config_tests.rs"]
mod tests;
