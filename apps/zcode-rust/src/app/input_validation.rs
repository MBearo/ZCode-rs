use super::Engine;
use crate::domain::MAX_TOOL_BYTES;
use anyhow::{Context, Result, bail};
use serde_json::Value;

impl Engine {
    pub(super) fn validate_selection(&self, p: &Value) -> Result<()> {
        if p["planEnabled"] == true
            || p["mode"].as_str().is_some_and(|mode| mode != "yolo")
            || p["followupMode"]
                .as_str()
                .is_some_and(|mode| !matches!(mode, "queue" | "guide"))
        {
            bail!("Unsupported core execution mode");
        }
        Ok(())
    }
    pub(super) fn validate_input(&self, p: &Value) -> Result<()> {
        self.validate_selection(p)?;
        if p.get("requestedDelivery")
            .is_some_and(|v| !matches!(v.as_str(), Some("auto" | "startNow" | "queue" | "guide")))
        {
            bail!("Invalid requested delivery");
        }
        // Composer 允许仅共享上下文发送；引用先严格解析，所属会话和状态仍由 admission 校验。
        let shared = crate::domain::shared_context::reference(p)?;
        p["text"]
            .as_str()
            .filter(|s| {
                s.len() <= MAX_TOOL_BYTES
                    && (!s.trim().is_empty()
                        || p["attachments"].as_array().is_some_and(|a| !a.is_empty())
                        || shared.is_some())
            })
            .context("Invalid input text")?;
        if let Some(refs) = p.get("attachments") {
            let refs = refs
                .as_array()
                .filter(|a| a.len() <= 16)
                .context("Invalid attachments")?;
            for item in refs {
                let attachment: crate::domain::protocol::AttachmentRef =
                    serde_json::from_value(item.clone())?;
                anyhow::ensure!(
                    !attachment.r#ref.is_empty()
                        && !attachment.r#ref.contains('\0')
                        && !attachment.file_name.is_empty()
                        && attachment.file_name.chars().count() <= 255
                        && !attachment.file_name.contains(['\0', '\r', '\n'])
                        && crate::domain::attachment_upload::valid_mime(&attachment.mime),
                    "Invalid attachment metadata"
                );
            }
        }
        for key in ["toolDisallowlist"] {
            if p.get(key)
                .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
            {
                bail!("Unsupported input field: {key}");
            }
        }
        for key in [
            "modelExecution",
            "automationId",
            "offPeakTaskId",
            "offPeakRunType",
            "browserAmbientContext",
        ] {
            if p.get(key).is_some() {
                bail!("Unsupported input field: {key}");
            }
        }
        if p.get("heldQueueDisposition")
            .is_some_and(|v| !matches!(v.as_str(), Some("clearQueueAndSend" | "keepQueueAndSend")))
        {
            bail!("Invalid held queue disposition");
        }
        if p.get("expectedHeldQueueItemIds").is_some_and(|v| {
            v.as_array().is_none_or(|a| {
                a.len() > crate::domain::MAX_QUEUE
                    || a.iter().any(|id| id.as_str().is_none_or(str::is_empty))
            })
        }) {
            bail!("Invalid expected queue ids");
        }
        Ok(())
    }
}
