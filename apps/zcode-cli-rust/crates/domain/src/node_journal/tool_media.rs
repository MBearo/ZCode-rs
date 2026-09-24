//! Node `persistToolResultMediaAttachments`: the media of a completed tool
//! result stored as artifacts and recorded on the tool part
//! (`state.attachments`, `metadata.modelContentLayout`). Spec
//! rust-m11-node-storage §5.3. Pure: the engine writes the artifacts.
use super::files::{self, Kind};
use serde_json::{Map, Value, json};

/// One media block of a tool result, stored as a Node artifact.
#[derive(Clone, Debug)]
pub struct MediaFile {
    pub mime: String,
    /// Node: the file block's `name`, else the source placeholder.
    pub filename: Option<String>,
    pub uri: String,
    /// Node `source.sizeBytes` (the original size the tool reported).
    pub size: Option<u64>,
}

/// The stored media of a tool result.
#[derive(Clone, Debug, Default)]
pub struct ToolMedia {
    pub files: Vec<MediaFile>,
    /// Node `PersistedToolMediaLayoutEntry[]`.
    pub layout: Vec<Value>,
}

/// Node `persistedToolMediaProjection` over Rust model content (text and
/// `_zcode_attachment` blocks): the layout and the media blocks in order, or
/// `None` when there is no media or a block Node would not persist.
pub fn projection(content: &Value) -> Option<(Vec<Value>, Vec<&Value>)> {
    let blocks = content.as_array().filter(|b| !b.is_empty())?;
    let (mut layout, mut media) = (vec![], vec![]);
    for block in blocks {
        match block["type"].as_str() {
            Some("text") => layout.push(json!({"type": "text", "text": block["text"]})),
            Some("_zcode_attachment")
                if files::kind(block["asset"]["mediaType"].as_str().unwrap_or(""))
                    != Kind::Other =>
            {
                layout.push(json!({"type": "attachment", "attachmentIndex": media.len()}));
                media.push(block);
            }
            _ => return None,
        }
    }
    (!media.is_empty()).then_some((layout, media))
}

/// Node's `filename` of a media block: a PDF file block's `name`, else the
/// source placeholder.
pub fn filename(block: &Value) -> Option<String> {
    let mime = block["asset"]["mediaType"].as_str().unwrap_or("");
    let key = if files::kind(mime) == Kind::Pdf {
        "name"
    } else {
        "placeholder"
    };
    block[key]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// Node `modelMessageContentToText` of a tool result's model content: the
/// tool part's `output`.
pub fn output_text(content: &Value) -> String {
    let Some(blocks) = content.as_array() else {
        return content.as_str().unwrap_or("").to_owned();
    };
    let shown = |mime: &str, name: Option<&str>| match name.filter(|n| !n.is_empty()) {
        Some(name) => format!("[Attached {mime}: {name}]"),
        None => format!("[Attached {mime}]"),
    };
    let texts = blocks.iter().map(|block| match block["type"].as_str() {
        Some("text") => block["text"].as_str().unwrap_or("").to_owned(),
        Some("_zcode_attachment") => {
            let mime = block["asset"]["mediaType"].as_str().unwrap_or("");
            let key = if files::kind(mime) == Kind::Pdf {
                "name"
            } else {
                "placeholder"
            };
            shown(mime, block[key].as_str().or(block["placeholder"].as_str()))
        }
        Some("image" | "video") => shown(
            block["mediaType"].as_str().unwrap_or(""),
            block["source"]["placeholder"].as_str(),
        ),
        Some("file") => match block["text"].as_str().filter(|t| !t.is_empty()) {
            Some(text) => text.to_owned(),
            None => shown(
                block["mediaType"].as_str().unwrap_or(""),
                block["name"]
                    .as_str()
                    .or(block["source"]["placeholder"].as_str()),
            ),
        },
        Some("resource_link") => format!(
            "[Resource: {}]",
            block["title"]
                .as_str()
                .or(block["name"].as_str())
                .or(block["uri"].as_str())
                .unwrap_or("")
        ),
        _ => String::new(),
    });
    texts
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

impl MediaFile {
    /// The `file` part in Node's key order (`id, messageID, sessionID, ...`).
    pub fn part(&self, id: &str, session: &str, message: &str) -> Value {
        let mut out = Map::new();
        out.insert("id".into(), id.into());
        out.insert("messageID".into(), message.into());
        out.insert("sessionID".into(), session.into());
        out.insert("type".into(), "file".into());
        out.insert("mime".into(), self.mime.clone().into());
        if let Some(filename) = &self.filename {
            out.insert("filename".into(), filename.clone().into());
        }
        out.insert("url".into(), self.uri.clone().into());
        let mut metadata = json!({"artifactUri": self.uri, "recoverability": "provider_ready",
            "storageKind": "artifact"});
        if let Some(size) = self.size {
            metadata["sizeBytes"] = size.into();
        }
        out.insert("metadata".into(), metadata);
        Value::Object(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media(mime: &str, name: &str, placeholder: &str) -> Value {
        json!({"type": "_zcode_attachment", "asset": {"path": "/a", "mediaType": mime, "totalBytes": 1},
            "name": name, "placeholder": placeholder})
    }

    #[test]
    fn projections_follow_node() {
        let pdf = json!([{"type": "text", "text": "PDF file read: a.pdf"},
            media("application/pdf", "a.pdf", "a.pdf")]);
        let (layout, blocks) = projection(&pdf).unwrap();
        assert_eq!(
            layout,
            [
                json!({"type": "text", "text": "PDF file read: a.pdf"}),
                json!({"type": "attachment", "attachmentIndex": 0})
            ]
        );
        assert_eq!(filename(blocks[0]).as_deref(), Some("a.pdf"));
        let image = media("image/png", "shot.png", "Read image");
        assert_eq!(filename(&image).as_deref(), Some("Read image"));
        assert!(projection(&json!([{"type": "text", "text": "x"}])).is_none());
        assert!(projection(&json!("text")).is_none());
        assert!(projection(&json!([media("text/plain", "a", "a")])).is_none());
        let part = MediaFile {
            mime: "image/png".into(),
            filename: Some("Read image".into()),
            uri: "zcode-artifact://s/tool-result-1".into(),
            size: Some(9),
        }
        .part("part_1", "s", "msg_1");
        let keys: Vec<&String> = part.as_object().unwrap().keys().collect();
        assert_eq!(
            keys,
            [
                "id",
                "messageID",
                "sessionID",
                "type",
                "mime",
                "filename",
                "url",
                "metadata"
            ]
        );
        assert_eq!(part["metadata"]["sizeBytes"], 9);
        assert_eq!(
            output_text(&pdf),
            "PDF file read: a.pdf\n\n[Attached application/pdf: a.pdf]"
        );
        assert_eq!(
            output_text(&json!([image])),
            "[Attached image/png: Read image]"
        );
        assert_eq!(output_text(&json!("plain")), "plain");
    }
}
