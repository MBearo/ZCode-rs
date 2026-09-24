//! Tool result media stored as Node artifacts before the tool part is
//! journaled (Node `persistToolResultMediaAttachments`, spec
//! rust-m11-node-storage §5.3).
use super::Engine;
use crate::contract::Event;
use crate::domain::node_journal::tool_media::{self, MediaFile, ToolMedia};
use crate::domain::session::StoredAttachment;
use anyhow::{Context, Result};

impl Engine {
    /// The stored media of a successful tool result with media blocks.
    /// Node fails the turn when the artifact cannot be written, so errors
    /// propagate.
    pub(super) async fn node_tool_media(
        &self,
        id: &str,
        event: &Event,
    ) -> Result<Option<ToolMedia>> {
        let Event::ToolDone {
            id: call,
            model_content: Some(content),
            failed: false,
            ..
        } = event
        else {
            return Ok(None);
        };
        let live = self
            .active
            .get(id)
            .is_some_and(|a| !a.cancel.is_cancelled());
        if !live || !self.journaled(id) {
            return Ok(None);
        }
        let Some((layout, blocks)) = tool_media::projection(content) else {
            return Ok(None);
        };
        let mut files = vec![];
        for (index, block) in blocks.into_iter().enumerate() {
            let asset: StoredAttachment =
                serde_json::from_value(block["asset"].clone()).context("Tool media unavailable")?;
            let bytes = self
                .store
                .read_attachment(&asset, 0, asset.total_bytes as usize)
                .await?;
            let name = format!("{call}-media-{}", index + 1);
            let (uri, _) = self
                .store
                .put_attachment(id, &name, &[bytes], &asset.media_type)
                .await?;
            files.push(MediaFile {
                mime: asset.media_type.clone(),
                filename: tool_media::filename(block),
                uri,
                size: block["sizeBytes"].as_u64(),
            });
        }
        Ok(Some(ToolMedia { files, layout }))
    }

    /// Node `emitFileMutationCheckpoint`: the before-change artifact of a
    /// successful file-mutating result; its URI for the checkpoint entry.
    pub(super) async fn node_checkpoint_artifact(
        &self,
        id: &str,
        event: &Event,
    ) -> Result<Option<String>> {
        use crate::domain::node_journal::checkpoint;
        let Event::ToolDone {
            id: call,
            checkpoint: Some(candidate),
            ..
        } = event
        else {
            return Ok(None);
        };
        let live = self
            .active
            .get(id)
            .is_some_and(|a| !a.cancel.is_cancelled());
        if !live || !self.journaled(id) {
            return Ok(None);
        }
        let tool = self.sessions[id]
            .node
            .turn
            .as_ref()
            .and_then(|t| t.step.as_ref())
            .and_then(|s| s.tools.iter().find(|t| &t.call == call))
            .map(|t| t.name.clone())
            .unwrap_or_default();
        let content = checkpoint::artifact(candidate, (call, &tool), self.clock.now());
        // Node：checkpoint 写入失败只记警告，不影响工具结果。
        match self
            .store
            .write_artifact(id, call, &content, checkpoint::CONTENT_TYPE)
            .await
        {
            Ok(uri) => Ok(Some(uri)),
            Err(error) => {
                tracing::warn!(error = %error, "workspace checkpoint write failed");
                Ok(None)
            }
        }
    }
}
