//! WebSearch without IO (Node `websearch.ts`, `websearch-results.ts`): input
//! validation, the provider-native tool, sources from the answer's markdown
//! links and the model-visible text.
use crate::js_string;
use crate::zod::{Schema, string};
use regex::Regex;
use serde_json::{Value, json};
use std::sync::OnceLock;

pub const SYSTEM: &str = "You are an assistant for performing a web search tool use.";
pub const CANCELLED: &str = "WebSearch was cancelled";
pub const UNSUPPORTED: &str = "Current model does not support native WebSearch";
/// Node `resultBudget`: `min(maxModelBytes 20 000, maxInlineBytes 10 000)`.
pub const MODEL_BYTES: usize = 10_000;
const MAX_LINKS: usize = 20;
const DEFAULT_MAX_USES: u64 = 8;

/// The provider-visible schema (`additionalProperties: false`, no `maxUses`).
fn schema() -> &'static Schema {
    static SCHEMA: OnceLock<Schema> = OnceLock::new();
    SCHEMA.get_or_init(|| {
        let domains = || Schema::Array(Box::new(string()), None);
        let query = Schema::String {
            trim: false,
            min: Some(2),
            max: None,
            format: None,
        };
        Schema::Object(vec![
            ("query", query, false),
            ("allowed_domains", domains(), true),
            ("blocked_domains", domains(), true),
        ])
    })
}

/// The InputValidationError text when `args` fails the schema.
pub fn validate(args: &Value) -> Result<(), String> {
    let (_, issues) = schema().run(Some(args));
    if issues.is_empty() {
        return Ok(());
    }
    Err(crate::tool_input::render("WebSearch", &issues))
}

/// Node `buildWebSearchProviderDescription` with the month text (`September 2026`).
pub fn description(template: &str, month: &str) -> String {
    template.replace("{currentMonth}", month)
}

/// The internal request's provider tool (`web_search_20260209`); empty
/// domain lists are left out (Node `toAiSdkTools`).
pub fn provider_tool(args: &Value) -> Value {
    let mut tool =
        json!({"type": "web_search_20260209", "name": "web_search", "max_uses": DEFAULT_MAX_USES});
    for key in ["allowed_domains", "blocked_domains"] {
        let list = args[key]
            .as_array()
            .filter(|l| !l.is_empty() && l.iter().all(Value::is_string));
        if let Some(list) = list {
            tool[key] = list.clone().into();
        }
    }
    tool
}

pub fn user_message(query: &str) -> String {
    format!("Perform a web search for the query: {query}")
}

/// Node `extractSourcesFromSummary`: markdown links (not images) to http(s).
fn summary_sources(summary: &str) -> Vec<(String, Option<String>)> {
    static LINK: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| {
        Regex::new(r"\[([^\]\n]+)\]\((https?://[^)\t\n\x0B\x0C\r \x{A0}\x{1680}\x{2000}-\x{200A}\x{2028}\x{2029}\x{202F}\x{205F}\x{3000}\x{FEFF}]+)\)").unwrap()
    });
    link.captures_iter(summary)
        .filter(|c| !summary[..c.get(0).unwrap().start()].ends_with('!'))
        .map(|c| {
            let title = js_string::trim(&c[1]);
            let title = (!title.is_empty()).then(|| title.to_owned());
            (js_string::trim(&c[2]).to_owned(), title)
        })
        .collect()
}

/// Node `buildWebSearchOutput` (results are always empty: the stream carries
/// no search result parts). `usage` is Node `ModelUsage` or null.
pub fn output(query: &str, text: &str, usage: Value, duration_ms: u64) -> Value {
    let summary = js_string::trim(text);
    let mut seen = std::collections::BTreeSet::new();
    let sources: Vec<Value> = summary_sources(summary)
        .into_iter()
        .filter(|(url, _)| seen.insert(url.to_lowercase()))
        .map(|(url, title)| match title {
            Some(title) => json!({"url": url, "title": title}),
            None => json!({"url": url}),
        })
        .collect();
    let mut output =
        json!({"query": query, "results": [], "sources": sources, "durationMs": duration_ms});
    if !summary.is_empty() {
        output["summary"] = summary.into();
    }
    if usage.as_object().is_some_and(|u| !u.is_empty()) {
        output["modelUsage"] = usage;
    }
    output
}

/// Node `formatWebSearchModelContent`.
pub fn model_content(output: &Value) -> String {
    let mut lines = vec![
        format!(
            "Web search results for query: \"{}\"",
            output["query"].as_str().unwrap_or("")
        ),
        String::new(),
    ];
    if let Some(summary) = output["summary"].as_str() {
        lines.extend(["Summary:".to_owned(), summary.to_owned(), String::new()]);
    }
    let links = output["sources"].as_array().map_or(&[][..], Vec::as_slice);
    lines.push("Links:".into());
    if links.is_empty() {
        lines.push("- No links found.".into());
    }
    for source in links.iter().take(MAX_LINKS) {
        let url = source["url"].as_str().unwrap_or("");
        lines.push(format!(
            "- [{}]({url})",
            source["title"].as_str().unwrap_or(url)
        ));
    }
    lines.extend([
        String::new(),
        "REMINDER: You MUST include the sources above in your response to the user using markdown hyperlinks.".to_owned(),
    ]);
    js_string::trim(&lines.join("\n")).to_owned()
}
