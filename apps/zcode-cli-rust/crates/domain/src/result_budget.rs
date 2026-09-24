//! Tool result budgets (Node `resultBudget` of each tool,
//! `result-serialization.ts` `serializeOutput` / `appendHookAdditionalContexts`,
//! `result-content-projection.ts`). Spec rust-m5-tools §2.3.
use serde_json::{Value, json};

const DEFAULT_MAX_BYTES: usize = 100_000;
const PREVIEW_CHARS: usize = 2_000;
const PERSISTED_OPEN: &str = "<persisted-output>";
const PERSISTED_CLOSE: &str = "</persisted-output>";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    Truncate,
    /// Oversized results are saved and replaced by a `<persisted-output>` envelope.
    Artifact,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Budget {
    /// `min(maxModelBytes, maxInlineBytes)` in UTF-8 bytes.
    pub max_bytes: usize,
    pub strategy: Strategy,
    /// Preview direction `tail` (Bash); otherwise `head`.
    pub tail: bool,
    /// The tool bounds its own result (Bash, TaskOutput); only hooks use the budget.
    pub owned: bool,
}

const fn budget(max_bytes: usize, strategy: Strategy) -> Budget {
    Budget {
        max_bytes,
        strategy,
        tail: false,
        owned: false,
    }
}

/// The budget of a tool (Node's per-handler declarations and the MCP one).
pub fn for_tool(name: &str) -> Budget {
    use Strategy::{Artifact, Truncate};
    match name {
        "Bash" => Budget {
            tail: true,
            owned: true,
            ..budget(30_000, Artifact)
        },
        "TaskOutput" => Budget {
            owned: true,
            ..budget(400_000, Artifact)
        },
        "Grep" => budget(20_000, Artifact),
        "Glob" | "WebFetch" => budget(100_000, Artifact),
        "Agent" | "Task" => budget(120_000, Artifact),
        "Read" => budget(256 * 1024, Truncate),
        "SendMessage" => budget(4_096, Truncate),
        "WebSearch" => budget(10_000, Truncate),
        mcp if mcp.starts_with("mcp__") => budget(50_000, Truncate),
        _ => budget(DEFAULT_MAX_BYTES, Truncate),
    }
}

/// Node `fitStringToBytes`: the longest head (or tail) of whole code points
/// within `max` UTF-8 bytes.
pub fn fit(value: &str, max: usize, tail: bool) -> &str {
    if value.len() <= max {
        return value;
    }
    if tail {
        let mut start = value.len() - max;
        while !value.is_char_boundary(start) {
            start += 1;
        }
        &value[start..]
    } else {
        let mut end = max;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        &value[..end]
    }
}

/// Node `fitContentWithSuffix`: content and suffix together within `max`
/// bytes, the suffix kept first.
pub fn fit_with_suffix(content: &str, max: usize, suffix: &str, tail: bool) -> String {
    if max == 0 {
        return String::new();
    }
    let suffix = fit(suffix, max, false);
    let remaining = max - suffix.len();
    if remaining == 0 {
        return suffix.to_owned();
    }
    format!("{}{suffix}", fit(content, remaining, tail))
}

/// What the serializer does with a successful result's text.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Inline,
    /// Save the text, then show [`persisted`]; fall back to [`truncated`].
    Persist,
    Truncate(String),
}

pub fn plan(text: &str, budget: Budget) -> Plan {
    if budget.owned || text.len() <= budget.max_bytes {
        Plan::Inline
    } else if budget.strategy == Strategy::Artifact {
        Plan::Persist
    } else {
        Plan::Truncate(truncated(text, budget))
    }
}

/// The truncated text with Node's resultBudget note.
pub fn truncated(text: &str, budget: Budget) -> String {
    let strategy = match budget.strategy {
        Strategy::Truncate => "truncate",
        Strategy::Artifact => "artifact",
    };
    let suffix = format!(
        "\n\n[Tool output truncated by resultBudget: originalBytes={}, maxModelBytes={}, strategy={strategy}]",
        text.len(),
        budget.max_bytes
    );
    fit_with_suffix(text, budget.max_bytes, &suffix, budget.tail)
}

/// Node `formatGenericPersistedOutputContent`.
pub fn persisted(text: &str, path: &str) -> String {
    crate::persisted_output::envelope(
        text,
        text.len() as u64,
        path,
        PREVIEW_CHARS,
        crate::persisted_output::decimal_bytes,
    )
}

fn is_persisted(text: &str) -> bool {
    text.starts_with(PERSISTED_OPEN) && text.contains(PERSISTED_CLOSE)
}

/// Node `appendHookAdditionalContexts`: `hook` joins the result within its
/// budget. A persisted envelope stays whole; structured blocks other than
/// text survive a truncation.
pub fn append_hook(content: &mut String, model: &mut Option<Value>, hook: &str, budget: Budget) {
    let suffix = format!("\n\n{hook}");
    let max = budget.max_bytes;
    if budget.strategy == Strategy::Artifact && is_persisted(content) {
        content.push_str(fit(&suffix, max, false));
        *model = None;
        return;
    }
    if content.len() + suffix.len() <= max {
        content.push_str(&suffix);
        if let Some(Value::Array(blocks)) = model {
            blocks.push(json!({"type":"text","text":hook}));
        }
        return;
    }
    let text = std::mem::take(content);
    *content = fit_with_suffix(&text, max, &suffix, budget.tail);
    let Some(Value::Array(blocks)) = model.take() else {
        return;
    };
    let preserved: Vec<Value> = blocks
        .iter()
        .filter(|b| b["type"] != "text")
        .cloned()
        .collect();
    if preserved.is_empty() {
        return;
    }
    let texts: Vec<&str> = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .filter(|t| !t.is_empty())
        .collect();
    let budgeted = fit_with_suffix(&texts.join("\n\n"), max, &suffix, budget.tail);
    let mut projected = preserved;
    if !budgeted.is_empty() {
        projected.push(json!({"type":"text","text":budgeted}));
    }
    *model = Some(Value::Array(projected));
}

#[cfg(test)]
#[path = "result_budget_tests.rs"]
mod tests;
