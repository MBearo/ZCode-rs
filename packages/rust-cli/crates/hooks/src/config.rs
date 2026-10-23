//! hooks 的设置与快照（docs/specs/rust-cli/hooks.md §2）。
//!
//! 结构是设置文件的 `hooks` 段（含 `args` 与 `continueOnBlock` 两个字段）。这里只解析入口层读好的 JSON，
//! 不做 IO；坏项跳过并给出警告（D1）。

use rust_cli_base::secret::EnvSnapshot;
use rust_cli_kernel_api::hooks::HookEventName;
use rust_cli_kernel_api::model::ModelSelection;
use serde_json::{Map, Value};

/// 设置来自哪里（setting source）。按合并顺序列出（排序即合并顺序）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum HookSource {
    User,
    Project,
    Local,
    Policy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HookShell {
    #[default]
    Bash,
    PowerShell,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandHook {
    pub command: String,
    /// exec 形式的参数（`args`）。
    pub args: Option<Vec<String>>,
    pub shell: HookShell,
    /// 秒。
    pub timeout: Option<f64>,
    /// `if` 条件（权限规则语法）。
    pub condition: Option<String>,
    /// `async` 或 `asyncRewake`：启动后不等待（D6）。
    pub background: bool,
    /// 插件目录：插件提供的 hook 才有（hooks.md §2.2 第 7 条）。
    pub plugin_root: Option<String>,
    /// Node 配置的 `onError: "deny"`：PreToolUse 里出错时拦下工具（hooks.md §4.1 末段）。`settings.json` 形状的设置没有这一项。
    pub deny_on_error: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PromptHook {
    pub prompt: String,
    pub model: Option<String>,
    pub timeout: Option<f64>,
    pub condition: Option<String>,
    pub continue_on_block: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentHook {
    pub prompt: String,
    pub model: Option<String>,
    pub timeout: Option<f64>,
    pub condition: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HttpHook {
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub allowed_env_vars: Vec<String>,
    pub timeout: Option<f64>,
    pub condition: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum HookCommand {
    Command(CommandHook),
    Prompt(PromptHook),
    Agent(AgentHook),
    Http(HttpHook),
}

impl HookCommand {
    /// Node 配置的 `onError: "deny"`（只有 command hook 有）。
    pub fn denies_on_error(&self) -> bool {
        matches!(self, Self::Command(hook) if hook.deny_on_error)
    }

    pub fn condition(&self) -> Option<&str> {
        match self {
            Self::Command(hook) => hook.condition.as_deref(),
            Self::Prompt(hook) => hook.condition.as_deref(),
            Self::Agent(hook) => hook.condition.as_deref(),
            Self::Http(hook) => hook.condition.as_deref(),
        }
    }

    pub fn timeout(&self) -> Option<f64> {
        match self {
            Self::Command(hook) => hook.timeout,
            Self::Prompt(hook) => hook.timeout,
            Self::Agent(hook) => hook.timeout,
            Self::Http(hook) => hook.timeout,
        }
    }

    /// 结果与提示里显示的命令文本。
    pub fn display(&self) -> String {
        match self {
            Self::Command(hook) => match &hook.args {
                Some(args) if !args.is_empty() => format!("{} {}", hook.command, args.join(" ")),
                _ => hook.command.clone(),
            },
            Self::Prompt(hook) => hook.prompt.clone(),
            Self::Agent(hook) => hook.prompt.clone(),
            Self::Http(hook) => hook.url.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct HookMatcher {
    pub matcher: Option<String>,
    pub hooks: Vec<HookCommand>,
}

/// 一份设置文件里与 hooks 有关的部分。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct HooksSettings {
    pub hooks: Vec<(HookEventName, Vec<HookMatcher>)>,
    pub disable_all_hooks: Option<bool>,
    pub allow_managed_hooks_only: Option<bool>,
    pub allowed_http_hook_urls: Option<Vec<String>>,
    pub http_hook_allowed_env_vars: Option<Vec<String>>,
}

fn strings(value: &Value) -> Option<Vec<String>> {
    value
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn text(object: &Map<String, Value>, key: &str) -> Option<String> {
    object.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn flag(object: &Map<String, Value>, key: &str) -> bool {
    object.get(key).and_then(Value::as_bool) == Some(true)
}

/// `timeout` 必须是正数（zod `.positive()`）。
fn timeout(object: &Map<String, Value>) -> Result<Option<f64>, String> {
    match object.get("timeout") {
        None => Ok(None),
        Some(value) => match value.as_f64() {
            Some(seconds) if seconds > 0.0 => Ok(Some(seconds)),
            _ => Err("timeout must be a positive number".to_owned()),
        },
    }
}

fn required(object: &Map<String, Value>, key: &str) -> Result<String, String> {
    text(object, key).ok_or_else(|| format!("missing string field `{key}`"))
}

fn parse_hook(value: &Value) -> Result<HookCommand, String> {
    let object = value.as_object().ok_or("hook is not an object")?;
    let condition = text(object, "if");
    let timeout = timeout(object)?;
    match object.get("type").and_then(Value::as_str) {
        Some("command") => {
            let args = match object.get("args") {
                None => None,
                Some(args) => Some(strings(args).ok_or("args must be an array of strings")?),
            };
            let shell = match object.get("shell").and_then(Value::as_str) {
                None | Some("bash") => HookShell::Bash,
                Some("powershell") => HookShell::PowerShell,
                Some(other) => return Err(format!("unknown shell `{other}`")),
            };
            Ok(HookCommand::Command(CommandHook {
                command: required(object, "command")?,
                args,
                shell,
                timeout,
                condition,
                background: flag(object, "async") || flag(object, "asyncRewake"),
                plugin_root: None,
                deny_on_error: false,
            }))
        }
        Some("prompt") => Ok(HookCommand::Prompt(PromptHook {
            prompt: required(object, "prompt")?,
            model: text(object, "model"),
            timeout,
            condition,
            continue_on_block: flag(object, "continueOnBlock"),
        })),
        Some("agent") => Ok(HookCommand::Agent(AgentHook {
            prompt: required(object, "prompt")?,
            model: text(object, "model"),
            timeout,
            condition,
        })),
        Some("http") => {
            let headers = match object.get("headers") {
                None => Vec::new(),
                Some(Value::Object(headers)) => headers
                    .iter()
                    .map(|(name, value)| {
                        value
                            .as_str()
                            .map(|value| (name.clone(), value.to_owned()))
                            .ok_or_else(|| format!("header `{name}` is not a string"))
                    })
                    .collect::<Result<_, _>>()?,
                Some(_) => return Err("headers must be an object".to_owned()),
            };
            Ok(HookCommand::Http(HttpHook {
                url: required(object, "url")?,
                headers,
                allowed_env_vars: object
                    .get("allowedEnvVars")
                    .and_then(strings)
                    .unwrap_or_default(),
                timeout,
                condition,
            }))
        }
        Some(other) => Err(format!("hook type `{other}` is not supported")),
        None => Err("hook has no type".to_owned()),
    }
}

fn parse_matchers(
    event: &str,
    value: &Value,
    warnings: &mut Vec<String>,
) -> Option<Vec<HookMatcher>> {
    let Some(items) = value.as_array() else {
        warnings.push(format!("hooks.{event}: not an array"));
        return None;
    };
    let mut matchers = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let Some(object) = item.as_object() else {
            warnings.push(format!("hooks.{event}[{index}]: not an object"));
            continue;
        };
        let Some(list) = object.get("hooks").and_then(Value::as_array) else {
            warnings.push(format!("hooks.{event}[{index}].hooks: not an array"));
            continue;
        };
        let mut hooks = Vec::new();
        for (position, hook) in list.iter().enumerate() {
            match parse_hook(hook) {
                Ok(hook) => hooks.push(hook),
                Err(error) => {
                    warnings.push(format!("hooks.{event}[{index}].hooks[{position}]: {error}"));
                }
            }
        }
        matchers.push(HookMatcher {
            matcher: text(object, "matcher"),
            hooks,
        });
    }
    Some(matchers)
}

impl HooksSettings {
    /// 从一份设置文件的 JSON 取出 hooks 相关的部分，返回值与警告。
    pub fn from_json(settings: &Value) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut parsed = Self::default();
        let Some(object) = settings.as_object() else {
            return (parsed, warnings);
        };
        if let Some(hooks) = object.get("hooks") {
            match hooks.as_object() {
                Some(events) => {
                    for (name, value) in events {
                        let Some(event) = HookEventName::parse(name) else {
                            warnings.push(format!("hooks.{name}: unknown hook event"));
                            continue;
                        };
                        if let Some(matchers) = parse_matchers(name, value, &mut warnings) {
                            parsed.hooks.push((event, matchers));
                        }
                    }
                }
                None => warnings.push("hooks: not an object".to_owned()),
            }
        }
        parsed.disable_all_hooks = object.get("disableAllHooks").and_then(Value::as_bool);
        parsed.allow_managed_hooks_only =
            object.get("allowManagedHooksOnly").and_then(Value::as_bool);
        parsed.allowed_http_hook_urls = object.get("allowedHttpHookUrls").and_then(strings);
        parsed.http_hook_allowed_env_vars = object.get("httpHookAllowedEnvVars").and_then(strings);
        (parsed, warnings)
    }

    pub(crate) fn matchers(&self, event: HookEventName) -> impl Iterator<Item = &HookMatcher> {
        self.hooks
            .iter()
            .filter(move |(name, _)| *name == event)
            .flat_map(|(_, matchers)| matchers)
    }
}

/// hooks 的带版本快照，由入口层组装（docs/specs/rust-cli/hooks.md §2.2）。
#[derive(Debug, Clone, PartialEq)]
pub struct HooksConfig {
    pub version: u64,
    /// 按 User → Project → Local → Policy 的顺序。
    pub sources: Vec<(HookSource, HooksSettings)>,
    /// 工作区已信任；为 false 时不运行任何 hook。
    pub trusted: bool,
    /// 入口捕获的进程环境：http hook 的 header 插值只从这里取值，只取 hook 的 `allowedEnvVars`（与策略的
    /// `httpHookAllowedEnvVars` 取交集）。`Debug` 只列变量名。
    pub env: EnvSnapshot,
    /// prompt hook 没有写 `model` 时用的模型（小模型槽位）。
    pub prompt_model: Option<ModelSelection>,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            version: 0,
            sources: Vec::new(),
            trusted: true,
            env: EnvSnapshot::default(),
            prompt_model: None,
        }
    }
}

#[cfg(test)]
#[path = "config_tests.rs"]
mod tests;
