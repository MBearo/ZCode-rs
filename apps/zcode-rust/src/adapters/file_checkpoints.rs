use super::checkpoint_blobs as blobs;
use crate::{
    contract::{Event, EventSink},
    domain::file_checkpoint::FileCheckpoint,
};
use anyhow::{Context, Result};
use std::path::Path;
use tokio_util::sync::CancellationToken;
pub(super) async fn prepare(
    root: &Path,
    path: &Path,
    name: &str,
    original: Option<&[u8]>,
    bytes: &[u8],
    sink: &EventSink,
    cancel: &CancellationToken,
) -> Result<()> {
    let before = match original {
        Some(bytes) => Some(blobs::save(root, bytes).await?),
        None => None,
    };
    let after = blobs::save(root, bytes).await?;
    let change = FileCheckpoint {
        id: super::id(),
        path: path.to_string_lossy().into_owned(),
        tool: name.into(),
        before,
        after,
        mode: blobs::mode(path).await?,
        row: 0,
        restored: false,
    };
    let (committed, receipt) = tokio::sync::oneshot::channel();
    sink.send(Event::FilePrepared { change, committed }).await?;
    tokio::select! {_=cancel.cancelled()=>anyhow::bail!("Cancelled"),r=receipt=>r.context("Checkpoint commit failed")?};
    Ok(())
}
