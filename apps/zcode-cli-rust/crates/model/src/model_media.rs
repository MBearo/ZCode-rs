use crate::contract::ModelFailure;
use serde_json::{Value, json};
type Result<T> = std::result::Result<T, ModelFailure>;
pub(super) fn responses(content: &Value, role: &str) -> Result<Vec<Value>> {
    content.as_array().ok_or_else(ModelFailure::invalid)?.iter().map(|p|Ok(match p["type"].as_str(){
        Some("text")=>json!({"type":if role=="assistant"{"output_text"}else{"input_text"},"text":p["text"]}),
        Some("image_url")=>json!({"type":"input_image","image_url":p["image_url"]["url"]}),
        Some("video_url")=>return Err(ModelFailure::new("attachment_unsupported", false)),
        Some("file")=>{let data=p["file"]["file_data"].as_str().ok_or_else(ModelFailure::invalid)?;if data.starts_with("data:"){json!({"type":"input_file","filename":p["file"]["filename"],"file_data":data})}else{json!({"type":"input_file","file_url":data})}},
        _=>return Err(ModelFailure::invalid()),
    })).collect()
}
pub(super) fn anthropic(content: &Value) -> Result<Vec<Value>> {
    content.as_array().ok_or_else(ModelFailure::invalid)?.iter().map(|p|Ok(match p["type"].as_str(){
        Some("text")=>p.clone(),
        Some("image_url")=>json!({"type":"image","source":source(p["image_url"]["url"].as_str().ok_or_else(ModelFailure::invalid)?)?}),
        Some("video_url")=>json!({"type":"video","source":source(p["video_url"]["url"].as_str().ok_or_else(ModelFailure::invalid)?)?}),
        Some("file")=>json!({"type":"document","source":source(p["file"]["file_data"].as_str().ok_or_else(ModelFailure::invalid)?)?}),
        _=>return Err(ModelFailure::invalid()),
    })).collect()
}
fn source(url: &str) -> Result<Value> {
    if let Some(data) = url.strip_prefix("data:") {
        let (mime, bytes) = data
            .split_once(";base64,")
            .ok_or_else(ModelFailure::invalid)?;
        Ok(json!({"type":"base64","media_type":mime,"data":bytes}))
    } else if url.starts_with("https://") || url.starts_with("http://") {
        Ok(json!({"type":"url","url":url}))
    } else {
        Err(ModelFailure::invalid())
    }
}
