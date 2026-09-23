use super::Engine;
use anyhow::{Result, ensure};
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
                let asset = self
                    .store
                    .put_attachment(&upload.chunks, &upload.meta.mime.to_ascii_lowercase())
                    .await?;
                let reference = format!("zcode-artifact://{}/{}", key.1, self.clock.id());
                self.sessions
                    .get_mut(&key.1)
                    .unwrap()
                    .attachments
                    .insert(reference.clone(), asset);
                // artifact 字节和 Session 归属都提交成功后才交付 ref；失败不能留下可用的成功回执。
                self.persist(&key.1, None).await?;
                self.uploads.committed(&key, reference.clone(), now);
                Ok(json!({"ref":reference}))
            }
        }
    }
}
