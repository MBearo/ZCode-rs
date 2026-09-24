use super::Engine;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

impl Engine {
    pub(super) async fn attachment_upload(&mut self, method: &str, p: &Value) -> Result<Value> {
        let key = crate::domain::attachment_upload::key(p)?;
        if method != "v4/attachment/begin" {
            ensure!(
                p.as_object().is_some_and(|o| o.keys().all(|k| matches!(
                    k.as_str(),
                    "connectionId" | "sessionId" | "uploadId"
                ) || (method
                    == "v4/attachment/chunk"
                    && matches!(k.as_str(), "chunkIndex" | "dataBase64")))),
                "Invalid attachment transaction fields"
            );
        }
        ensure!(self.sessions.contains_key(&key.1), "Session unavailable");
        let now = self.clock.now();
        self.uploads.prune(now);
        match method {
            "v4/attachment/begin" => self.uploads.begin(p, now),
            "v4/attachment/chunk" => self.uploads.chunk(p, now),
            "v4/attachment/abort" => {
                if self
                    .uploads
                    .0
                    .get(&key)
                    .is_some_and(|u| u.committed.is_none())
                {
                    self.uploads.0.remove(&key);
                }
                Ok(json!({}))
            }
            _ => {
                let upload = self.uploads.validated(&key)?;
                if let Some(reference) = &upload.committed {
                    return Ok(json!({"ref":reference}));
                }
                // Node writePromptAttachment：上传内容即 data URL 产物，ref 为其 URI。
                let call = crate::domain::node_ids::upload_call(now, &self.clock.id());
                let (reference, asset) = self
                    .store
                    .put_attachment(
                        &key.1,
                        &call,
                        &upload.chunks,
                        &upload.meta.mime.to_ascii_lowercase(),
                    )
                    .await?;
                self.register_attachment(&key.1, reference.clone(), asset)?;
                // artifact 字节和 Session 归属都提交成功后才交付 ref；失败不能留下可用的成功回执。
                self.persist(&key.1, None).await?;
                self.uploads.committed(&key, reference.clone(), now);
                Ok(json!({"ref":reference}))
            }
        }
    }

    /// Registers stored artifact bytes on the session under their reference:
    /// the one registration path of uploads and legacy inline attachments.
    /// The caller persists.
    pub(super) fn register_attachment(
        &mut self,
        id: &str,
        reference: String,
        asset: crate::domain::session::StoredAttachment,
    ) -> Result<()> {
        self.sessions
            .get_mut(id)
            .context("Session unavailable")?
            .attachments
            .insert(reference, asset);
        Ok(())
    }
}
