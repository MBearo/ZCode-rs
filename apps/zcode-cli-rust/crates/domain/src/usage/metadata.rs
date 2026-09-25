//! Node's normalized `ModelUsage` and the tool registry facts the usage
//! tables record (spec rust-m9-usage-logs §2.3).
use serde_json::{Map, Value};

/// Node `normalizeUsage` over the adapter's usage object (`prompt_tokens`,
/// `completion_tokens`, `total_tokens`, cache and reasoning details, and the
/// provider's `server_tool_use`). Members Node leaves undefined are omitted;
/// `Null` when the provider reported no usage.
pub fn model_usage(raw: &Value) -> Value {
    if raw.as_object().is_none_or(|u| u.is_empty()) {
        return Value::Null;
    }
    let mut out = Map::new();
    let input = raw["prompt_tokens"].as_u64();
    let output = raw["completion_tokens"].as_u64();
    let total = raw["total_tokens"]
        .as_u64()
        .or(input.zip(output).map(|(i, o)| i + o));
    for (key, value) in [
        ("inputTokens", input),
        ("outputTokens", output),
        ("totalTokens", total),
        (
            "cacheReadTokens",
            raw["prompt_tokens_details"]["cached_tokens"].as_u64(),
        ),
        (
            "cacheWriteTokens",
            raw["prompt_tokens_details"]["cache_write_tokens"].as_u64(),
        ),
        (
            "reasoningTokens",
            raw["completion_tokens_details"]["reasoning_tokens"].as_u64(),
        ),
    ] {
        if let Some(value) = value {
            out.insert(key.into(), value.into());
        }
    }
    let server = &raw["server_tool_use"];
    let fetch = server["web_fetch_requests"].as_u64();
    let search = server["web_search_requests"].as_u64();
    if fetch.is_some() || search.is_some() {
        let mut tools = Map::new();
        if let Some(fetch) = fetch {
            tools.insert("webFetchRequests".into(), fetch.into());
        }
        if let Some(search) = search {
            tools.insert("webSearchRequests".into(), search.into());
        }
        out.insert("serverToolUse".into(), Value::Object(tools));
    }
    Value::Object(out)
}

/// Node `ModelToolMetadata` facts of a tool: `(sideEffectScope, readOnly, destructive)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ToolMeta {
    pub scope: &'static str,
    pub read_only: bool,
    pub destructive: bool,
}

const fn meta(scope: &'static str, read_only: bool) -> ToolMeta {
    ToolMeta {
        scope,
        read_only,
        destructive: false,
    }
}

/// The registry metadata Node records for `name` (built-in tools; MCP tools
/// from their annotations, `readOnlyHint` and `destructiveHint`).
pub fn tool_meta(name: &str, annotations: Option<&Value>) -> Option<ToolMeta> {
    if name.starts_with("mcp__") {
        let hints = annotations.unwrap_or(&Value::Null);
        // 宿主 node_repl 的 js 能执行本机代码：Node 记为 system，其他 MCP 为 network。
        let scope = if name == "mcp__node_repl__js" {
            "system"
        } else {
            "network"
        };
        return Some(ToolMeta {
            scope,
            read_only: hints["readOnlyHint"] == true,
            destructive: hints["destructiveHint"] == true,
        });
    }
    Some(match name {
        "Read" | "Glob" | "Grep" | "TaskOutput" | "TodoRead" => meta("none", true),
        "Write" | "Edit" => meta("workspace", false),
        "Bash" => meta("system", false),
        "WebFetch" | "WebSearch" => meta("network", true),
        "TodoWrite" | "Skill" | "Agent" | "Task" | "ReadSessionContext" => meta("session", true),
        "SendMessage" | "TaskStop" | "EnterPlanMode" | "ExitPlanMode" => meta("session", false),
        "AskUserQuestion" => meta("userInteraction", true),
        _ => return None,
    })
}
