use super::Engine;
use crate::{contract::ModelIdentity, domain::session::StoredAttachment};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;

impl Engine {
    pub(super) async fn prepare_attachments(
        &self,
        id: &str,
        p: &mut Value,
        selection: &ModelIdentity,
    ) -> Result<BTreeMap<String, StoredAttachment>> {
        let mut assets = BTreeMap::new();
        let Some(refs) = p.get_mut("attachments").and_then(Value::as_array_mut) else {
            return Ok(assets);
        };
        let model = if let Some(registry) = &self.registry {
            registry.resolve(selection)?
        } else {
            self.model.clone().context("Model unavailable")?
        };
        let properties = model.format_properties();
        for item in refs {
            let original = item["ref"].as_str().context("Attachment ref required")?;
            let mime = item["mime"]
                .as_str()
                .context("Attachment MIME required")?
                .to_ascii_lowercase();
            let capability = if mime.starts_with("image/") {
                Some("supportsImage")
            } else if mime == "application/pdf" {
                Some("supportsPdf")
            } else if mime.starts_with("video/") {
                Some("supportsVideo")
            } else if mime.starts_with("audio/") {
                Some("supportsAudio")
            } else {
                None
            };
            if let Some(capability) = capability {
                ensure!(
                    properties["inputFormat"][capability] == true,
                    "Attachment format is unsupported by selected model"
                );
            }
            ensure!(
                !mime.starts_with("audio/"),
                "Audio attachment input is not implemented"
            );
            let (reference, asset) = if original.starts_with("zcode-artifact://") {
                let asset = self
                    .sessions
                    .get(id)
                    .and_then(|s| s.attachments.get(original))
                    .context("Attachment does not belong to this session")?;
                ensure!(
                    asset.media_type.eq_ignore_ascii_case(&mime)
                        && item["bytes"] == asset.total_bytes,
                    "Attachment metadata does not match committed content"
                );
                (original.to_owned(), asset.clone())
            } else {
                ensure!(
                    !original.contains("://") || original.starts_with("file://"),
                    "Unsupported attachment source"
                );
                let path = if original.starts_with("file://") {
                    original.to_owned()
                } else {
                    std::path::Path::new(&self.workspace_path)
                        .join(original)
                        .to_string_lossy()
                        .into_owned()
                };
                let asset = self.store.snapshot_attachment(&path, &mime).await?;
                (format!("zcode-artifact://{id}/{}", self.clock.id()), asset)
            };
            if mime == "application/pdf" {
                ensure!(
                    self.store.read_attachment(&asset, 0, 5).await? == b"%PDF-",
                    "Attachment PDF is invalid"
                );
            }
            item["ref"] = reference.clone().into();
            item["mime"] = mime.into();
            item["bytes"] = asset.total_bytes.into();
            item.as_object_mut().unwrap().remove("previewRef");
            assets.insert(reference, asset);
        }
        Ok(assets)
    }
    pub(super) fn input_content(&self, id: &str, p: &Value) -> Result<Value> {
        let text = p["text"].as_str().context("Input text missing")?;
        let refs = p["attachments"].as_array().filter(|a| !a.is_empty());
        let Some(refs) = refs else {
            return Ok(text.into());
        };
        let mut content = vec![];
        if !text.is_empty() {
            content.push(json!({"type":"text","text":text}));
        }
        for item in refs {
            let asset = self.sessions[id]
                .attachments
                .get(item["ref"].as_str().unwrap())
                .context("Attachment snapshot unavailable")?;
            content.push(json!({"type":"_zcode_attachment","asset":asset,"name":item["fileName"]}));
        }
        Ok(content.into())
    }
}
