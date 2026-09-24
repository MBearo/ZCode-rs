//! What clients and the model see of a hook (Node `display-metadata.ts`,
//! `hook-flow.ts` context text, `runtime/methods/hooks.ts` lifecycle text and
//! the product projection's execution names).
use super::{HookEvent, Program, Registration, input, truncate_utf16, utf16_prefix};
use serde_json::{Value, json};
use std::sync::LazyLock;

const MASK: &str = "••••";
const KEY: &str =
    "(?:access[_-]?token|api[_-]?key|credential|password|private[_-]?key|secret|token)";

/// Node `sanitizeHookDisplayText` patterns, in order, with their JS flags.
static REDACTIONS: LazyLock<Vec<(regress::Regex, &'static str)>> = LazyLock::new(|| {
    let id = format!("(?:[A-Za-z0-9]+[_-])*{KEY}(?:[_-][A-Za-z0-9]+)*");
    let value = "(?:\"[^\"]*\"|'[^']*'|[^\\s,\"']+)";
    let patterns = [
        (
            r"([a-z][a-z0-9+.-]*:\/\/)[^\s/:@]+:[^\s/@]+@".to_owned(),
            "iu",
            "$1\u{0}:\u{0}@",
        ),
        (
            r#"(authorization\s*:\s*(?:basic|bearer)\s+)(?:"[^"]*"|'[^']*'|[^\s,;"']+)"#.to_owned(),
            "iu",
            "$1\u{0}",
        ),
        (
            format!(r#"([?&](?:authorization|{id})=)[^&\s"']+"#),
            "iu",
            "$1\u{0}",
        ),
        (
            format!(r"(\bauthorization\b\s*=\s*){value}"),
            "iu",
            "$1\u{0}",
        ),
        (
            format!(r#"((?:"{id}"|'{id}')\s*[:=]\s*){value}"#),
            "iu",
            "$1\u{0}",
        ),
        (
            format!(r"(^|[^A-Za-z0-9_])({id}\s*[:=]\s*){value}"),
            "imu",
            "$1$2\u{0}",
        ),
        (
            format!(r#"((?:--)?{id})(\s+)(?:"[^"]*"|'[^']*'|[^\s]+)"#),
            "iu",
            "$1$2\u{0}",
        ),
    ];
    patterns
        .into_iter()
        .map(|(pattern, flags, template)| {
            let regex = regress::Regex::with_flags(&pattern, flags).expect("valid redaction");
            (regex, template)
        })
        .collect()
});

/// JS `String.prototype.replace` with a global regex and `$n` templates
/// (`\0` in the template stands for the mask).
fn replace_all(regex: &regress::Regex, text: &str, template: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = 0;
    for m in regex.find_iter(text) {
        out.push_str(&text[last..m.range().start]);
        let mut chars = template.chars().peekable();
        while let Some(c) = chars.next() {
            match c {
                '\u{0}' => out.push_str(MASK),
                '$' if chars.peek().is_some_and(char::is_ascii_digit) => {
                    let group = chars.next().unwrap().to_digit(10).unwrap() as usize;
                    if let Some(range) = m.group(group) {
                        out.push_str(&text[range]);
                    }
                }
                c => out.push(c),
            }
        }
        last = m.range().end;
    }
    out.push_str(&text[last..]);
    out
}

/// Node `sanitizeHookDisplayText`: masks credentials in URLs, headers and
/// key/value pairs.
pub fn sanitize(value: &str) -> String {
    let mut text = value.to_owned();
    for (regex, template) in REDACTIONS.iter() {
        text = replace_all(regex, &text, template);
    }
    text
}

/// Node `quoteCommandPart`.
fn quote(part: &str) -> String {
    let plain = !part.is_empty()
        && part
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "_./:@%+=,-".contains(c));
    if plain {
        part.to_owned()
    } else {
        Value::from(part).to_string()
    }
}

/// Node `createHookExecutionDescriptor`: the display command has variables
/// expanded (errors keep the raw text) and credentials masked.
pub fn descriptor(hook: &Registration, hook_input: &Value) -> Value {
    let cwd = hook_input["cwd"].as_str().unwrap_or("");
    let expand = |value: &str| {
        input::expand(value, hook.plugin.as_ref(), hook_input, cwd)
            .unwrap_or_else(|_| value.to_owned())
    };
    let (command, kind, background) = match &hook.program {
        Program::Command {
            command,
            background,
            ..
        } => (expand(command), "command", *background),
        Program::Process { command, args } => {
            let parts: Vec<String> = std::iter::once(command)
                .chain(args)
                .map(|part| quote(&expand(part)))
                .collect();
            (parts.join(" "), "process", false)
        }
    };
    let mut out = json!({
        "clientVisible": true,
        "commandDisplay": sanitize(&command),
        "executionMode": if background { "background" } else { "foreground" },
        "executionType": kind,
    });
    let plugin = hook.plugin.as_ref();
    if let Some(p) = plugin {
        out["pluginId"] = p.id.clone().into();
        out["pluginName"] = p.name.clone().into();
    }
    out["sourceKind"] = hook.source_kind.as_str().into();
    let path = plugin
        .and_then(|p| p.source_path.clone())
        .or_else(|| hook.source_path.clone());
    if let Some(path) = path {
        out["sourcePath"] = path.into();
    }
    if let Some(message) = hook.status_message.as_ref().filter(|m| !m.is_empty()) {
        out["statusMessage"] = message.clone().into();
    }
    out["timeoutMs"] = hook.timeout_ms.into();
    out
}

const SCRIPT_RUNNERS: [&str; 12] = [
    "bash",
    "bun",
    "deno",
    "node",
    "node.exe",
    "powershell",
    "pwsh",
    "python",
    "python3",
    "ruby",
    "sh",
    "zsh",
];

/// Tokens as `/"(?:\\.|[^"])*"|'[^']*'|\S+/gu` finds them.
fn tokens(text: &str) -> Vec<&str> {
    static TOKEN: LazyLock<regress::Regex> = LazyLock::new(|| {
        regress::Regex::with_flags(r#""(?:\\.|[^"])*"|'[^']*'|\S+"#, "u").unwrap()
    });
    TOKEN.find_iter(text).map(|m| &text[m.range()]).collect()
}

/// Node `unquoteHookDisplayToken`.
fn unquote(token: &str) -> String {
    // JS `slice(1, -1)`：单个引号字符得到空串。
    let inner = || {
        token
            .get(1..token.len().saturating_sub(1))
            .unwrap_or("")
            .to_owned()
    };
    if token.starts_with('"') && token.ends_with('"') {
        return serde_json::from_str::<String>(token).unwrap_or_else(|_| inner());
    }
    if token.starts_with('\'') && token.ends_with('\'') {
        return inner();
    }
    token.into()
}

fn base_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Node `hookCommandLabel`: the executable, or `runner · script`.
fn command_label(display: &str) -> Option<String> {
    let tokens = tokens(display);
    let executable = unquote(tokens.first()?);
    let executable = base_name(&executable);
    if executable.is_empty() {
        return None;
    }
    let script = tokens
        .get(1)
        .filter(|_| SCRIPT_RUNNERS.contains(&executable.to_lowercase().as_str()))
        .map(|token| unquote(token))
        .filter(|s| !s.is_empty() && !s.starts_with('-'));
    let Some(script) = script else {
        return Some(executable.into());
    };
    let name = base_name(&script);
    Some(if name.is_empty() {
        executable.into()
    } else {
        format!("{executable} · {name}")
    })
}

/// Node `hookExecutionDisplayName` (product projection).
pub fn display_name(descriptor: &Value, hook_index: u64) -> String {
    let label = descriptor["commandDisplay"]
        .as_str()
        .and_then(command_label);
    let status = descriptor["statusMessage"]
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let plugin = descriptor["pluginName"].as_str().filter(|s| !s.is_empty());
    status
        .map(str::to_owned)
        .or_else(|| match (plugin, &label) {
            (Some(plugin), Some(label)) => Some(format!("{plugin} · {label}")),
            (Some(plugin), None) => Some(plugin.into()),
            (None, label) => label.clone(),
        })
        .unwrap_or_else(|| format!("Hook #{}", hook_index + 1))
}

/// Node `formatHookAdditionalContexts` (tool results).
pub fn tool_contexts(contexts: &[String]) -> String {
    std::iter::once("[Hook additional context]".to_owned())
        .chain(
            contexts
                .iter()
                .enumerate()
                .map(|(i, c)| format!("#{}\n{c}", i + 1)),
        )
        .collect::<Vec<_>>()
        .join("\n")
}

/// Node `formatLifecycleHookAdditionalContextBody` (SessionStart,
/// UserPromptSubmit, Stop), cut at 24000 UTF-16 units.
pub fn lifecycle_body(event: HookEvent, contexts: &[String]) -> Option<String> {
    if contexts.is_empty() {
        return None;
    }
    let body = contexts
        .iter()
        .enumerate()
        .map(|(i, c)| format!("#{}\n{c}", i + 1))
        .collect::<Vec<_>>()
        .join("\n\n");
    let text = format!("{} hook additional context: \n{body}", event.as_str());
    Some(truncate_utf16(&text, 24_000, "..."))
}

/// Node `sanitizeHookDiagnostics` for one preview: trimmed, masked, 4000 units.
pub fn diagnostic(value: Option<&str>) -> Option<String> {
    let trimmed = value?.trim();
    (!trimmed.is_empty()).then(|| utf16_prefix(&sanitize(trimmed), 4000).to_owned())
}
