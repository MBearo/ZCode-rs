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
}
