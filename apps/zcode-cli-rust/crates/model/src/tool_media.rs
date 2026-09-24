//! Tool results with media on the wire (Node `tool-result-media-projection.ts`):
//! the text form of a result's blocks, and the media that follow the tool
//! results as a user message where the protocol has no tool-result media.
use serde_json::{Value, json};

/// Parts `materialize` produced from media (`image_url`, `video_url`, `file`).
pub(super) fn is_media(part: &Value) -> bool {
    matches!(
        part["type"].as_str(),
        Some("image_url" | "video_url" | "file")
    )
}

pub(super) fn is_video(part: &Value) -> bool {
    part["type"] == "video_url"
}

fn data_url(part: &Value) -> &str {
    match part["type"].as_str() {
        Some("image_url") => part["image_url"]["url"].as_str(),
        Some("video_url") => part["video_url"]["url"].as_str(),
        Some("file") => part["file"]["file_data"].as_str(),
        _ => None,
    }
    .unwrap_or("")
}

/// Node `modelMessageContentBlockToText` for one block.
fn block_text(part: &Value) -> String {
    if !is_media(part) {
        return part["text"].as_str().unwrap_or("").to_owned();
    }
    let mime = data_url(part)
        .strip_prefix("data:")
        .and_then(|rest| rest.split([';', ',']).next())
        .unwrap_or("application/octet-stream");
    match part["_zcode_placeholder"]
        .as_str()
        .filter(|p| !p.is_empty())
    {
        Some(placeholder) => format!("[Attached {mime}: {placeholder}]"),
        None => format!("[Attached {mime}]"),
    }
}

/// Node `modelMessageContentToText`: non-empty block texts joined by a blank line.
pub(super) fn text_form(parts: &[Value]) -> String {
    parts
        .iter()
        .map(block_text)
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Node `toToolResultMediaUserParts`: the deferred media after an intro line.
pub(super) fn deferred(tool: &str, media: Vec<Value>) -> Option<Value> {
    if media.is_empty() {
        return None;
    }
    let mut parts =
        vec![json!({"type": "text", "text": format!("Tool result media from {tool}:")})];
    parts.extend(media.into_iter().map(clean));
    Some(json!({"role": "user", "content": parts}))
}

/// A part without the runtime's private keys.
pub(super) fn clean(mut part: Value) -> Value {
    if let Some(object) = part.as_object_mut() {
        object.retain(|k, _| !k.starts_with("_zcode_"));
    }
    part
}

/// Tool names by call id, from the assistant messages' tool calls.
pub(super) fn tool_names(messages: &[Value]) -> std::collections::HashMap<String, String> {
    messages
        .iter()
        .filter_map(|m| m["tool_calls"].as_array())
        .flatten()
        .map(|c| {
            (
                c["id"].as_str().unwrap_or("").to_owned(),
                c["function"]["name"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect()
}

/// Chat Completions: every media block becomes text in the tool message and
/// follows the run of tool messages as a user message.
pub(super) fn chat(messages: Vec<Value>) -> Vec<Value> {
    let names = tool_names(&messages);
    let mut out = Vec::with_capacity(messages.len());
    let mut pending = vec![];
    for mut message in messages {
        let tool = message["role"] == "tool";
        if !tool {
            out.append(&mut pending);
        }
        if tool && let Some(parts) = message["content"].as_array() {
            let media: Vec<Value> = parts.iter().filter(|p| is_media(p)).cloned().collect();
            let name = names
                .get(message["tool_call_id"].as_str().unwrap_or(""))
                .map_or("", String::as_str);
            message["content"] = text_form(parts).into();
            pending.extend(deferred(name, media));
        }
        out.push(message);
    }
    out.append(&mut pending);
    out
}
