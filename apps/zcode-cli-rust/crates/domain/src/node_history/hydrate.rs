//! Node `hydrateMessageHistoryFromSession` over the active messages.
use super::attachment::{ArtifactReader, file_block, prompt_attachment};
use super::branch::truthy;
use super::entries::{Entry, metadata};
use super::{Record, reminders};
use serde_json::{Map, Value, json};

const INTERRUPTED: &str = "[Tool execution was interrupted before resume]";

/// Node `RuntimeInputPresentationSchema`.
const PRESENTATIONS: [&str; 7] = [
    "user_steer",
    "coordinator_steer",
    "coordinator_input",
    "subagent_reply_steer",
    "subagent_reply",
    "task_notification_steer",
    "task_notification",
];

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hydrated {
    pub entries: Vec<Entry>,
    /// The stored message each entry came from (parallel to `entries`).
    pub sources: Vec<String>,
    pub interrupted_tools: usize,
}

/// Node `runtimeInputMetadata`.
fn presentation_metadata(value: &Value) -> Option<Value> {
    let presentation = value.as_str().filter(|p| PRESENTATIONS.contains(p))?;
    let source = if presentation == "user_steer" {
        "real_user"
    } else {
        "legacy_synthetic"
    };
    Some(metadata(source, Some(presentation)))
}

/// Node `runtimeMessageMetadataFromPartMetadata`.
fn runtime_metadata(part_metadata: &Value) -> Option<Value> {
    let runtime = part_metadata
        .get("runtimeMessage")
        .filter(|r| r.is_object())?;
    if let Some(presented) = presentation_metadata(&runtime["inputPresentation"]) {
        return Some(presented);
    }
    let source = runtime["source"].as_str()?;
    (matches!(source, "real_user" | "legacy_synthetic" | "todo_reminder")
        || reminders::known(source))
    .then(|| metadata(source, None))
}

/// Node `metadataFromSyntheticTextPart`.
fn synthetic_metadata(part: &Value) -> Value {
    let source = part["metadata"]["source"].as_str();
    if matches!(source, Some("background_task" | "subagent_message")) {
        return metadata("legacy_synthetic", None);
    }
    if let Some(persisted) = runtime_metadata(&part["metadata"]) {
        return persisted;
    }
    let mapped = match source {
        Some("subagent") => "queued_system_notification",
        Some("todo_reminder") => "todo_reminder",
        Some("goal-continuation") => "target_continuation",
        Some("rewind" | "fork") => "rewind_notice",
        Some(other) if reminders::known(other) => other,
        _ => "legacy_synthetic",
    };
    metadata(mapped, None)
}

fn wrapped(text: &str) -> bool {
    text.trim_start().starts_with("<system-reminder")
}

/// Node `syntheticSystemReminderAttachmentFromTextPart`.
fn synthetic_attachment(part: &Value) -> Option<Entry> {
    let text = part["text"].as_str().unwrap_or("");
    if part["synthetic"] != true || text.trim().is_empty() || wrapped(text) {
        return None;
    }
    let source = synthetic_metadata(part)["source"].as_str()?.to_owned();
    reminders::restorable(&source).then(|| Entry::Attachment {
        source,
        content: text.to_owned(),
    })
}

/// Node `textPartToProviderText`.
fn provider_text(part: &Value) -> String {
    let text = part["text"].as_str().unwrap_or("").to_owned();
    if part["synthetic"] != true || wrapped(&text) {
        return text;
    }
    let runtime = runtime_metadata(&part["metadata"]);
    let runtime_source = runtime.as_ref().and_then(|m| m["source"].as_str());
    let source = part["metadata"]["source"].as_str();
    if runtime_source == Some("task_status") && source != Some("background_task") {
        return reminders::wrap("task_status", &text);
    }
    if runtime_source == Some("queued_system_notification") || source == Some("subagent") {
        return reminders::wrap("queued_system_notification", &text);
    }
    text
}

fn visible_text(part: &Value) -> bool {
    part["type"] == "text" && part["ignored"] != true
}

/// Node `metadataFromUserParts`.
fn user_metadata(parts: &[Value]) -> Value {
    let real_text = parts
        .iter()
        .any(|p| visible_text(p) && p["synthetic"] != true);
    let structured = parts
        .iter()
        .any(|p| matches!(p["type"].as_str(), Some("file" | "agent")));
    if real_text || structured {
        return metadata("real_user", None);
    }
    match parts
        .iter()
        .find(|p| visible_text(p) && p["synthetic"] == true)
    {
        Some(part) => synthetic_metadata(part),
        None => metadata("real_user", None),
    }
}

/// Node `modelMessageContentBlockToText` emptiness (placeholders are non-empty).
fn block_has_text(block: &Value) -> bool {
    match block["type"].as_str() {
        Some("text") => !block["text"].as_str().unwrap_or("").trim().is_empty(),
        Some("reasoning") => false,
        _ => true,
    }
}

/// Node `userEntriesFromParts`.
fn user_entries(parts: &[Value], artifacts: ArtifactReader) -> Vec<Entry> {
    let (mut attachments, mut prompt, mut media) = (vec![], vec![], vec![]);
    let (mut synthetic, mut reminders_) = (vec![], vec![]);
    for part in parts {
        match part["type"].as_str() {
            Some("text") if part["ignored"] != true => match synthetic_attachment(part) {
                Some(entry) => synthetic.push(entry),
                None => prompt.push(json!({"type": "text", "text": provider_text(part)})),
            },
            Some("file") => {
                let block = file_block(part, artifacts);
                if block["type"] == "text"
                    && let Some(body) = prompt_attachment(part, &block)
                {
                    reminders_.push(Entry::Attachment {
                        source: "prompt_attachment".into(),
                        content: body,
                    });
                    continue;
                }
                if matches!(block["type"].as_str(), Some("image" | "video")) {
                    media.push(block);
                } else {
                    attachments.push(block);
                }
            }
            Some("agent") => prompt.push(json!({"type": "text",
                "text": format!("[Selected agent: {}]", part["name"].as_str().unwrap_or(""))})),
            _ => {}
        }
    }
    let preserve = !attachments.is_empty() || !media.is_empty();
    let blocks: Vec<Value> = attachments.into_iter().chain(prompt).chain(media).collect();
    let has_content = blocks.iter().any(block_has_text);
    let content = if preserve || blocks.iter().any(|b| b["type"] != "text") {
        Value::Array(blocks)
    } else {
        let texts: Vec<&str> = blocks
            .iter()
            .filter_map(|b| b["text"].as_str())
            .filter(|t| !t.is_empty())
            .collect();
        Value::String(texts.join("\n\n"))
    };
    let user = user_metadata(parts);
    let envelope = has_content || (user["source"] == "real_user" && !reminders_.is_empty());
    if !envelope && synthetic.is_empty() && reminders_.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    if envelope {
        out.push(Entry::Message {
            message: json!({"role": "user", "content": content}),
            metadata: Some(user),
            tokens: None,
        });
    }
    out.extend(synthetic);
    out.extend(reminders_);
    out
}

/// Node `persistedTokenUsageBaseline(...) !== undefined`: a positive input window.
fn usage_baseline(tokens: &Value) -> bool {
    let positive = |v: &Value| {
        v.as_f64()
            .filter(|n| n.is_finite())
            .map(f64::floor)
            .filter(|n| *n > 0.0)
    };
    if positive(&tokens["input"]).is_some() {
        return true;
    }
    if let Some(total) = positive(&tokens["total"]) {
        let output = tokens["output"]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map_or(0.0, f64::floor);
        return total - output > 0.0;
    }
    let cache = |k: &str| {
        tokens["cache"][k]
            .as_f64()
            .filter(|n| n.is_finite() && *n >= 0.0)
            .map_or(0.0, f64::floor)
    };
    cache("read") + cache("write") > 0.0
}

/// Node `selectToolPartsForHistory`: the last part per call id, in declaration
/// order when every part has one.
fn tool_parts(parts: &[Value]) -> Vec<&Value> {
    let tools: Vec<&Value> = parts.iter().filter(|p| p["type"] == "tool").collect();
    let mut latest: Vec<&Value> = tools
        .iter()
        .enumerate()
        .filter(|(i, part)| {
            !tools[i + 1..]
                .iter()
                .any(|later| later["callID"] == part["callID"])
        })
        .map(|(_, part)| *part)
        .collect();
    // 任一 part 缺声明序号（旧记录或混合版本）时整组保留原序。
    if latest.iter().all(|p| p.get("declarationIndex").is_some()) {
        latest.sort_by_key(|p| p["declarationIndex"].as_i64().unwrap_or(0));
    }
    latest
}

/// Node `providerToolNameFromPart`.
fn tool_name(part: &Value) -> Value {
    match part["metadata"].get("providerToolName") {
        Some(Value::String(name)) => name.as_str().into(),
        _ => part["tool"].clone(),
    }
}

/// Node `dedupeParts`: one part per id at its first position, last write wins.
fn dedupe(parts: &[Value]) -> Vec<Value> {
    let mut map: Map<String, Value> = Map::new();
    for part in parts {
        map.insert(part["id"].as_str().unwrap_or("").to_owned(), part.clone());
    }
    map.into_iter().map(|(_, part)| part).collect()
}

fn assistant(record: &Record, parts: &[Value], out: &mut Hydrated) {
    let text: Vec<&str> = parts
        .iter()
        .filter(|p| visible_text(p))
        .filter_map(|p| p["text"].as_str())
        .collect();
    let text = text.join("\n\n");
    let reasoning: Vec<Value> = parts
        .iter()
        .filter(|p| p["type"] == "reasoning")
        .map(|p| {
            let mut block = json!({"type": "reasoning", "text": p["text"]});
            if truthy(p.get("metadata")) {
                block["providerOptions"] = p["metadata"].clone();
            }
            block
        })
        .collect();
    let tools = tool_parts(parts);
    let tokens = &record.info["tokens"];
    if text.trim().is_empty() && reasoning.is_empty() && tools.is_empty() && !usage_baseline(tokens)
    {
        return;
    }
    let content = if reasoning.is_empty() {
        Value::String(text)
    } else {
        let mut blocks = reasoning;
        if !text.is_empty() {
            blocks.push(json!({"type": "text", "text": text}));
        }
        Value::Array(blocks)
    };
    let calls: Vec<Value> = tools
        .iter()
        .map(|p| json!({"id": p["callID"], "name": tool_name(p), "input": p["state"]["input"]}))
        .collect();
    let mut message = json!({"role": "assistant", "content": content, "toolCalls": calls});
    let info = &record.info;
    if truthy(info.get("modelId")) && truthy(info.get("providerId")) {
        message["providerId"] = info["providerId"].clone();
        message["modelId"] = info["modelId"].clone();
    }
    out.entries.push(Entry::Message {
        message,
        metadata: None,
        tokens: tokens.is_object().then(|| tokens.clone()),
    });
    for part in tools {
        let state = &part["state"];
        let (content, failed) = match state["status"].as_str() {
            // 媒体附件（attachments + modelContentLayout）随 M11.4 的产物读取一起接入。
            Some("completed") => (state["output"].clone(), false),
            Some("error") => match state["metadata"]["modelContent"].as_str() {
                Some(model) => (model.into(), true),
                None => (state["error"].clone(), true),
            },
            _ => {
                out.interrupted_tools += 1;
                (INTERRUPTED.into(), true)
            }
        };
        out.entries.push(Entry::Message {
            message: json!({"role": "tool", "content": content, "toolCallId": part["callID"],
                "toolName": tool_name(part), "isError": failed}),
            metadata: None,
            tokens: None,
        });
    }
}

/// Node `hydrateMessageHistoryFromSession` over already active messages.
pub fn hydrate(active: &[impl AsRef<Record>], artifacts: ArtifactReader) -> Hydrated {
    let mut out = Hydrated::default();
    for record in active {
        let record = record.as_ref();
        hydrate_record(record, artifacts, &mut out);
        let id = record.id().to_owned();
        out.sources.resize(out.entries.len(), id);
    }
    out
}

fn hydrate_record(record: &Record, artifacts: ArtifactReader, out: &mut Hydrated) {
    {
        let parts = dedupe(&record.parts);
        let info = &record.info;
        if info["role"] != "user" {
            assistant(record, &parts, out);
            return;
        }
        let shared = &info["metadata"]["sharedContextStatus"];
        if info["source"] == "shared_context" && !shared.is_null() && *shared != "attached" {
            // 共享上下文在首次发送前只是候选，不能提前进入模型上下文。
            return;
        }
        let visible: Vec<&Value> = parts
            .iter()
            .filter(|p| !(p["type"] == "text" && p["ignored"] == true))
            .collect();
        if let [part] = visible.as_slice()
            && part["type"] == "text"
            && let Some(entry) = synthetic_attachment(part)
        {
            out.entries.push(entry);
            return;
        }
        let mut entries = user_entries(&parts, artifacts);
        if let Some(presented) = presentation_metadata(&info["metadata"]["inputPresentation"]) {
            for entry in &mut entries {
                if let Entry::Message { metadata, .. } = entry {
                    *metadata = Some(presented.clone());
                }
            }
        }
        out.entries.extend(entries);
    }
}

impl AsRef<Record> for Record {
    fn as_ref(&self) -> &Record {
        self
    }
}
