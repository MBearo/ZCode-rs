//! Node `createBashPerformanceTelemetry` (`tool-perf.ts`, `bash-output.ts`):
//! the command facts of a Bash call for telemetry. Only the category, a
//! registry executable name and counts leave the process, never the command.
use regex::Regex;
use serde_json::{Map, Value, json};
use std::sync::OnceLock;

/// Node `COMMAND_CLASSIFY_PREFIX_CHARS` and `MAX_COMMAND_IDENTITY_PARSE_CHARS`.
const CLASSIFY_PREFIX: usize = 2048;
const IDENTITY_LIMIT: usize = 8 * 1024;

/// Node `classifyCommand` over a bounded, lower-cased prefix.
pub(crate) fn category(command: &str) -> &'static str {
    static RULES: OnceLock<Vec<(Regex, &'static str)>> = OnceLock::new();
    let rules = RULES.get_or_init(|| {
        [
            (
                r"\b(?:npm|pnpm|yarn|bun)\s+(?:test|run\s+test|vitest|jest)\b",
                "test",
            ),
            (
                r"\b(?:npm|pnpm|yarn|bun)\s+(?:install|add|update|remove)\b",
                "package",
            ),
            (r"\bgit\b", "git"),
            (r"\b(?:rg|grep|find|fd)\b", "search"),
            (
                r"\b(?:npm|pnpm|yarn|bun)\s+(?:run\s+)?(?:build|compile)\b",
                "build",
            ),
            (r"\b(?:curl|wget|gh\s+api)\b", "network"),
        ]
        .into_iter()
        // JS 的 \b 只看 ASCII 单词字符（即使带 u 标志）。
        .map(|(rule, name)| {
            let rule = rule.replace(r"\b", r"(?-u:\b)");
            (Regex::new(&rule).expect("command category"), name)
        })
        .collect()
    });
    let prefix = crate::domain::js_string::utf16_prefix(command, CLASSIFY_PREFIX);
    let normalized = prefix
        .trim_start_matches(crate::domain::js_string::is_space)
        .to_lowercase();
    if normalized.is_empty() {
        return "empty";
    }
    rules
        .iter()
        .find(|(rule, _)| rule.is_match(&normalized))
        .map_or("other", |(_, name)| name)
}

/// Node `classifySafeCommandIdentity`: `(count, name)`; a static
/// executable of the public registry, else a low-cardinality bucket.
pub(crate) fn identity(command: &str) -> (Option<usize>, String) {
    if command.encode_utf16().count() > IDENTITY_LIMIT {
        return (None, "other".into());
    }
    let analysis = zcode_cli_bash::analyze(command);
    let count = analysis.commands.len();
    if crate::domain::js_string::trim(command).is_empty() {
        return (Some(0), "empty".into());
    }
    if analysis.has_parse_errors || analysis.has_unsupported_syntax || analysis.has_dynamic_words {
        return (Some(count), "other".into());
    }
    if count != 1 {
        let name = if count > 1 { "compound" } else { "other" };
        return (Some(count), name.into());
    }
    let name = analysis.commands[0].name.as_str();
    let executable = name.rsplit(['/', '\\']).next().unwrap_or("").to_lowercase();
    let name = if zcode_cli_bash::registered(&executable) {
        executable
    } else {
        "other".into()
    };
    (Some(count), name)
}

/// The `command` detail of a finished Bash run (`data` is the run result);
/// `run_ms` is `None` for a background launch (`status: backgrounded`).
pub(crate) fn detail(command: &str, data: &Value, run_ms: Option<u64>) -> Value {
    let mut facts = Map::new();
    if let Some(ms) = run_ms {
        facts.insert("runMs".into(), ms.into());
        // Node 的 firstOutputMs 只来自流式进度计时；没有时 noOutputMs 即运行时长。
        facts.insert("noOutputMs".into(), ms.into());
        if let Some(code) = data["exitCode"].as_i64() {
            facts.insert("exitCode".into(), code.into());
        }
        facts.insert("timedOut".into(), (data["timedOut"] == true).into());
        let bytes =
            data["stdoutBytes"].as_u64().unwrap_or(0) + data["stderrBytes"].as_u64().unwrap_or(0);
        facts.insert("outputBytes".into(), bytes.into());
    }
    facts.insert("category".into(), category(command).into());
    let (count, name) = identity(command);
    facts.insert("name".into(), name.into());
    if let Some(count) = count {
        facts.insert("count".into(), count.into());
    }
    let status = match run_ms {
        None => "backgrounded",
        Some(_) if data["timedOut"] == true => "timed_out",
        Some(_) if data["cancelled"] == true => "cancelled",
        Some(_) => data["status"]
            .as_str()
            .filter(|s| matches!(*s, "completed" | "failed" | "spawn_error"))
            .unwrap_or("completed"),
    };
    facts.insert("status".into(), status.into());
    json!({"kind": "command", "command": facts})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn categories_and_identities_follow_node() {
        assert_eq!(category("  pnpm run test --filter x"), "test");
        assert_eq!(category("git status"), "git");
        assert_eq!(category("echo hi"), "other");
        assert_eq!(category("   "), "empty");
        assert_eq!(identity("echo hi"), (Some(1), "echo".into()));
        assert_eq!(identity("/usr/bin/ECHO hi"), (Some(1), "echo".into()));
        assert_eq!(identity("echo a && ls"), (Some(2), "compound".into()));
        assert_eq!(identity("./build.sh"), (Some(1), "other".into()));
        assert_eq!(identity("echo $HOME"), (Some(1), "other".into()));
        assert_eq!(identity(""), (Some(0), "empty".into()));
    }

    #[test]
    fn a_finished_run_reports_its_command_facts() {
        let data = json!({"status": "completed", "exitCode": 0, "stdoutBytes": 9, "stderrBytes": 2,
            "timedOut": false, "cancelled": false});
        assert_eq!(
            detail("echo hi", &data, Some(20)),
            json!({"kind": "command", "command": {"runMs": 20, "noOutputMs": 20, "exitCode": 0,
                "timedOut": false, "outputBytes": 11, "category": "other", "name": "echo",
                "count": 1, "status": "completed"}})
        );
        let background = detail("sleep 5", &Value::Null, None);
        assert_eq!(background["command"]["status"], "backgrounded");
        assert!(background["command"].get("runMs").is_none());
    }
}
