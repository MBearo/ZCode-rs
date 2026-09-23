use super::Engine;
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
impl Engine {
    pub(super) async fn attachment_query(&self, method: &str, p: &Value) -> Result<Value> {
        let session = self
            .sessions
            .get(p["sessionId"].as_str().context("Session required")?)
            .context("Session unavailable")?;
        let reference = p["ref"].as_str().context("Attachment ref required")?;
        let target = p.get("target");
        let index = p["attachmentIndex"].as_u64();
        ensure!(
            target.is_some() == index.is_some(),
            "Attachment target/index required together"
        );
        ensure!(
            !method.starts_with("v4/conversation/") || target.is_some(),
            "Attachment target required"
        );
        // 只读当前 session 已发布的附件；不能把任意 file:// 或 artifact URI 变成文件读取权限。
        let authorized = session.rows.iter().any(|row| {
            if target
                .is_some_and(|t| row["rowId"] != t["rowId"] || row["entityId"] != t["entityId"])
            {
                return false;
            }
            row["attachments"].as_array().is_some_and(|a| match index {
                Some(i) => a.get(i as usize).is_some_and(|a| a["ref"] == reference),
                None => a.iter().any(|a| a["ref"] == reference),
            })
        });
        ensure!(authorized, "fault.attachment.readNotAuthorized");
        let asset = session
            .attachments
            .get(reference)
            .context("fault.attachment.sourceUnavailable")?;
        if method == "v4/attachment/previewSource" {
            return Ok(json!({"kind":"chunked"}));
        }
        if method.ends_with("Stat") {
            return Ok(json!({"mediaType":asset.media_type,"totalBytes":asset.total_bytes}));
        }
        if method == "v4/attachment/read" {
            ensure!(
                asset.media_type.starts_with("image/")
                    || asset.media_type.starts_with("video/")
                    || asset.media_type == "application/pdf",
                "Unsupported preview MIME"
            );
        }
        let offset = p["offset"].as_u64().context("Invalid attachment offset")?;
        let limit = p["limit"]
            .as_u64()
            .filter(|n| *n > 0 && *n <= 512 * 1024)
            .context("Invalid attachment limit")?;
        ensure!(offset <= asset.total_bytes, "Invalid attachment offset");
        let bytes = self
            .store
            .read_attachment(asset, offset, limit as usize)
            .await?;
        use base64::Engine as _;
        let next = offset + bytes.len() as u64;
        Ok(
            json!({"dataBase64":base64::engine::general_purpose::STANDARD.encode(&bytes),"mediaType":asset.media_type,"totalBytes":asset.total_bytes,"nextOffset":if next<asset.total_bytes{Some(next)}else{None}}),
        )
    }
}
