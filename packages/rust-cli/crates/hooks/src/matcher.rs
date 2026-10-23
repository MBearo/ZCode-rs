//! 选出一次事件要运行的 hook（docs/specs/rust-cli/hooks.md §3.3）。
//!
//! 步骤：match query 过滤 →
//! 展开 → 按类型分组去重 → `if` 条件 → SessionStart / Setup 不跑 http hook。

use rust_cli_kernel_api::hooks::HookEventName;
// 旧工具名：与权限规则共用 kernel-api 的一张表（F58）。
use rust_cli_kernel_api::tool::{LEGACY_TOOL_NAMES as LEGACY, canonical_tool_name};
use serde_json::Value;

use crate::config::{HookCommand, HookMatcher, HookShell};

/// matcher 精确列表允许 `,`、空格与 `-` 的事件。
const EXTENDED: &[HookEventName] = &[
    HookEventName::PreToolUse,
    HookEventName::PostToolUse,
    HookEventName::PostToolUseFailure,
    HookEventName::PermissionRequest,
    HookEventName::PermissionDenied,
    HookEventName::UserPromptExpansion,
    HookEventName::SessionStart,
    HookEventName::SessionEnd,
    HookEventName::Setup,
    HookEventName::PreCompact,
    HookEventName::PostCompact,
    HookEventName::PreModelSwitch,
    HookEventName::PostModelSwitch,
    HookEventName::Notification,
    HookEventName::SubagentStart,
    HookEventName::SubagentStop,
    HookEventName::Elicitation,
    HookEventName::ElicitationResult,
    HookEventName::ConfigChange,
    HookEventName::InstructionsLoaded,
    HookEventName::DirectoryAdded,
];

/// `if` 条件能求值的事件。
const TOOL_EVENTS: &[HookEventName] = &[
    HookEventName::PreToolUse,
    HookEventName::PostToolUse,
    HookEventName::PostToolUseFailure,
    HookEventName::PermissionRequest,
    HookEventName::PermissionDenied,
];

fn normalize(name: &str) -> &str {
    canonical_tool_name(name)
}

fn legacy_names(name: &str) -> impl Iterator<Item = &'static str> + '_ {
    LEGACY
        .iter()
        .filter(move |(_, new)| *new == name)
        .map(|(old, _)| *old)
}

fn field(payload: &Value, key: &str) -> Option<String> {
    payload.get(key).and_then(Value::as_str).map(str::to_owned)
}

/// 事件的 match query；None 表示不按 matcher 过滤。
pub(crate) fn match_query(event: HookEventName, payload: &Value) -> Option<String> {
    use HookEventName as E;
    match event {
        E::PreToolUse
        | E::PostToolUse
        | E::PostToolUseFailure
        | E::PermissionRequest
        | E::PermissionDenied => field(payload, "tool_name"),
        E::UserPromptExpansion => field(payload, "command_name"),
        E::SessionStart | E::ConfigChange | E::DirectoryAdded => field(payload, "source"),
        E::Setup | E::PreCompact | E::PostCompact => field(payload, "trigger"),
        E::PreModelSwitch | E::PostModelSwitch => field(payload, "to_model"),
        E::Notification => field(payload, "notification_type"),
        E::SessionEnd => field(payload, "reason"),
        E::StopFailure => field(payload, "error"),
        E::SubagentStart | E::SubagentStop => field(payload, "agent_type"),
        E::Elicitation | E::ElicitationResult => field(payload, "mcp_server_name"),
        E::InstructionsLoaded => field(payload, "load_reason"),
        E::FileChanged => field(payload, "file_path").map(|path| {
            path.rsplit(['/', '\\'])
                .next()
                .unwrap_or_default()
                .to_owned()
        }),
        E::PostToolBatch
        | E::UserPromptSubmit
        | E::Stop
        | E::TeammateIdle
        | E::TaskCreated
        | E::TaskCompleted
        | E::WorktreeCreate
        | E::WorktreeRemove
        | E::CwdChanged
        | E::MessageDisplay => None,
    }
}

/// 精确列表；matcher 不是纯列表时为 None。
fn exact_list(matcher: &str, extended: bool) -> Option<Vec<String>> {
    let plain = matcher.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || c == '_'
            || c == '|'
            || (extended && matches!(c, ',' | ' ' | '-'))
    });
    if !plain {
        return None;
    }
    let parts: Vec<&str> = if extended {
        matcher.split(['|', ',']).collect()
    } else {
        matcher.split('|').collect()
    };
    Some(
        parts
            .into_iter()
            .map(str::trim)
            .filter(|part| !part.is_empty())
            .map(|part| normalize(part).to_owned())
            .collect(),
    )
}

/// matcher 是否匹配 match query。
pub(crate) fn matches(event: HookEventName, matcher: Option<&str>, query: &str) -> bool {
    let Some(matcher) = matcher.filter(|m| !m.is_empty() && *m != "*") else {
        return true;
    };
    if let Some(list) = exact_list(matcher, EXTENDED.contains(&event)) {
        return list.iter().any(|item| item == query)
            || legacy_names(query).any(|old| list.iter().any(|item| item == old));
    }
    match regress::Regex::new(matcher) {
        Ok(regex) => {
            regex.find(query).is_some() || legacy_names(query).any(|old| regex.find(old).is_some())
        }
        Err(_) => {
            tracing::debug!(matcher, "invalid regex pattern in hook matcher");
            false
        }
    }
}

/// 去重键（command 按 shell、命令、`args` 与 `if` 条件；prompt / agent 按 `prompt\0if`）。
fn dedup_key(hook: &HookCommand) -> String {
    let condition = hook.condition().unwrap_or_default();
    match hook {
        HookCommand::Command(command) => {
            let shell = match command.shell {
                HookShell::Bash => "bash",
                HookShell::PowerShell => "powershell",
            };
            let args = command.args.as_ref().map_or_else(
                || "null".to_owned(),
                |args| Value::from(args.clone()).to_string(),
            );
            // 键带插件目录：两个插件里同样的命令不互相去重。
            let root = command.plugin_root.as_deref().unwrap_or_default();
            format!(
                "{root}\0command\0{shell}\0{}\0{args}\0{condition}",
                command.command
            )
        }
        HookCommand::Prompt(prompt) => format!("{}\0{condition}", prompt.prompt),
        HookCommand::Agent(agent) => format!("{}\0{condition}", agent.prompt),
        HookCommand::Http(http) => format!("http\0{}\0{condition}", http.url),
    }
}

fn group(hook: &HookCommand) -> usize {
    match hook {
        HookCommand::Command(_) => 0,
        HookCommand::Prompt(_) => 1,
        HookCommand::Agent(_) => 2,
        HookCommand::Http(_) => 3,
    }
}

/// 组内去重：保留第一次出现的位置、用最后一次出现的定义（JS `new Map(entries)`）。
fn dedup(hooks: Vec<HookCommand>) -> Vec<HookCommand> {
    let mut groups: [Vec<(String, HookCommand)>; 4] = Default::default();
    for hook in hooks {
        let key = dedup_key(&hook);
        let bucket = &mut groups[group(&hook)];
        match bucket.iter_mut().find(|(known, _)| *known == key) {
            Some(slot) => slot.1 = hook,
            None => bucket.push((key, hook)),
        }
    }
    groups.into_iter().flatten().map(|(_, hook)| hook).collect()
}

/// 权限规则的通配：`*` 匹配任意串；`prefix:*` 是前缀规则。
fn wildcard(pattern: &str, text: &str) -> bool {
    if let Some(prefix) = pattern.strip_suffix(":*") {
        return text == prefix || text.starts_with(&format!("{prefix} "));
    }
    let parts: Vec<&str> = pattern.split('*').collect();
    if parts.len() == 1 {
        return pattern == text;
    }
    let (first, last) = (parts[0], parts[parts.len() - 1]);
    if text.len() < first.len() + last.len() || !text.starts_with(first) || !text.ends_with(last) {
        return false;
    }
    let mut at = first.len();
    let end = text.len() - last.len();
    for part in &parts[1..parts.len() - 1] {
        match text[at..end].find(part) {
            Some(found) => at += found + part.len(),
            None => return false,
        }
    }
    true
}

/// `if` 条件（D5）：工具名相同；有括号内容时只对 Bash 的 `command` 求值。
fn condition_matches(condition: &str, tool: &str, input: &Value) -> bool {
    let (name, content) = match condition.split_once('(') {
        Some((name, rest)) => (name.trim(), rest.strip_suffix(')')),
        None => (condition.trim(), None),
    };
    if normalize(name) != normalize(tool) {
        return false;
    }
    match content {
        None | Some("") => true,
        Some(content) if normalize(tool) == "Bash" => input
            .get("command")
            .and_then(Value::as_str)
            .is_some_and(|command| wildcard(content, command.trim())),
        Some(_) => false,
    }
}

/// 一次事件要运行的 hook，按配置的顺序。
pub(crate) fn select(
    event: HookEventName,
    matchers: &[HookMatcher],
    payload: &Value,
) -> Vec<HookCommand> {
    let query = match_query(event, payload);
    let hooks: Vec<HookCommand> = matchers
        .iter()
        .filter(|matcher| {
            query
                .as_deref()
                .is_none_or(|query| matches(event, matcher.matcher.as_deref(), query))
        })
        .flat_map(|matcher| matcher.hooks.iter().cloned())
        .collect();
    let tool = payload.get("tool_name").and_then(Value::as_str);
    let input = payload.get("tool_input").unwrap_or(&Value::Null);
    dedup(hooks)
        .into_iter()
        .filter(|hook| match hook.condition() {
            None => true,
            Some(condition) => match tool.filter(|_| TOOL_EVENTS.contains(&event)) {
                Some(tool) => condition_matches(condition, tool, input),
                None => false,
            },
        })
        .filter(|hook| {
            !(matches!(hook, HookCommand::Http(_))
                && matches!(event, HookEventName::SessionStart | HookEventName::Setup))
        })
        .collect()
}

#[cfg(test)]
#[path = "matcher_tests.rs"]
mod tests;
