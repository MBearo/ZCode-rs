//! Prompt attachments at admission: checked against the model, stored, and
//! resolved the way Node resolves them (spec rust-m11-node-storage §5.3).
use super::Engine;
use crate::domain::node_journal::files::{self, Kind, Media, NodeFile, Placement};
use crate::{contract::ModelIdentity, domain::session::StoredAttachment};
use anyhow::{Context, Result, bail, ensure};
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
        for (index, item) in refs.iter_mut().enumerate() {
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
            let (reference, mut asset) = if original.starts_with("zcode-artifact://") {
                let asset = self.uploaded(id, original).await?;
                ensure!(
                    asset.media_type.eq_ignore_ascii_case(&mime)
                        && item["bytes"] == asset.total_bytes,
                    "Attachment metadata does not match committed content"
                );
                if mime == "application/pdf" {
                    ensure!(
                        self.store.read_attachment(&asset, 0, 5).await? == b"%PDF-",
                        "Attachment PDF is invalid"
                    );
                }
                (original.to_owned(), asset)
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
                self.store
                    .local_attachment(id, index, (original, &path), &mime)
                    .await?
            };
            if asset.node.is_none() {
                let name = item["fileName"].as_str().unwrap_or("");
                let (node, prepared) = self.resolve_upload(&reference, &asset, name, index).await?;
                asset.node = Some(node);
                asset.prepared = prepared.map(Box::new);
            }
            item["ref"] = reference.clone().into();
            item["mime"] = mime.into();
            item["bytes"] = asset.total_bytes.into();
            item.as_object_mut().unwrap().remove("previewRef");
            assets.insert(reference, asset);
        }
        Ok(assets)
    }

    /// An uploaded attachment of session `id`: in memory, or (a resumed or
    /// Node-written session) found by its URI in the artifact root.
    async fn uploaded(&self, id: &str, reference: &str) -> Result<StoredAttachment> {
        if let Some(asset) = self
            .sessions
            .get(id)
            .and_then(|s| s.attachments.get(reference))
        {
            return Ok(asset.clone());
        }
        let owner = reference
            .strip_prefix("zcode-artifact://")
            .and_then(|rest| rest.split('/').next());
        if owner == Some(id)
            && let Some(asset) = self.store.attachment_of(reference).await?
        {
            return Ok(asset);
        }
        bail!("Attachment does not belong to this session")
    }

    /// Node `resolveTurnAttachment` of an uploaded (`zcode-artifact://`) ref,
    /// and the prepared asset requests send instead (an uploaded image).
    async fn resolve_upload(
        &self,
        reference: &str,
        asset: &StoredAttachment,
        name: &str,
        index: usize,
    ) -> Result<(NodeFile, Option<StoredAttachment>)> {
        let mime = asset.media_type.as_str();
        match files::kind(mime) {
            Kind::Image => {
                return self
                    .store
                    .uploaded_image(reference, asset, name, index)
                    .await;
            }
            Kind::Other => {}
            _ => {
                let file = files::media(Media {
                    uri: reference,
                    mime,
                    bytes: asset.total_bytes,
                    file_name: name,
                    index,
                    local: None,
                    image: None,
                });
                return Ok((file, None));
            }
        }
        // Node：非媒体上传按 UTF-8 解码进入 prompt；超过 64 KiB、空内容或读失败只留占位。
        let total = asset.total_bytes;
        if total == 0 || total > files::INLINE_TEXT_MAX_BYTES {
            return Ok((files::unread_upload(index), None));
        }
        let file = match self.store.read_attachment(asset, 0, total as usize).await {
            Ok(bytes) => files::inline_text(name, &String::from_utf8_lossy(&bytes)),
            Err(_) => files::unread_upload(index),
        };
        Ok((file, None))
    }

    /// Node `buildRuntimeUserEntriesFromTurn`: the user content and the
    /// `prompt_attachment` reminder bodies that follow it.
    pub(super) fn input_content(&self, id: &str, p: &Value) -> Result<(Value, Vec<String>)> {
        let text = p["text"].as_str().context("Input text missing")?;
        let refs = p["attachments"].as_array().filter(|a| !a.is_empty());
        let Some(refs) = refs else {
            return Ok((text.into(), vec![]));
        };
        let mut placed = vec![];
        for item in refs {
            let asset = self.sessions[id]
                .attachments
                .get(item["ref"].as_str().unwrap())
                .context("Attachment snapshot unavailable")?;
            let lazy = |asset: &StoredAttachment| {
                // 上传图片的请求发送准备后的字节（Node 用准备后的 data URL），产物仍是原图。
                let mut bare = asset.prepared.as_deref().unwrap_or(asset).clone();
                bare.node = None;
                bare.prepared = None;
                json!({"type":"_zcode_attachment","asset":bare,"name":item["fileName"]})
            };
            placed.push(match &asset.node {
                Some(node) if node.block["type"] == "text" => {
                    (node.placement(), node.block.clone())
                }
                Some(node) => (node.placement(), lazy(asset)),
                None => (Placement::Real, lazy(asset)),
            });
        }
        Ok(files::user_content(text, placed))
    }
}
