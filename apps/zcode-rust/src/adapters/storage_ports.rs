use super::storage::{Operation, Store};
use crate::domain::session::Session;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::oneshot;

#[async_trait::async_trait]
impl crate::contract::SessionStore for Store {
    async fn load_index(&self, workspace: &str) -> Result<BTreeMap<String, Value>> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Operation::Index(workspace.into(), tx)).await?;
        rx.await?
    }
    async fn lookup_ack(&self, workspace: &str, key: &str) -> Result<Option<Value>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Operation::Ack(workspace.into(), key.into(), tx))
            .await?;
        rx.await?
    }
    async fn list_sessions(
        &self,
        params: &crate::domain::session_listing::ListParams,
        owner: (&str, &str),
    ) -> Result<Vec<crate::domain::session_listing::SessionListing>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Operation::List(
                params.clone(),
                (owner.0.into(), owner.1.into()),
                tx,
            ))
            .await?;
        rx.await?
    }
    async fn load_session(&self, workspace: &str, id: &str) -> Result<Option<Session>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Operation::LoadSession(workspace.into(), id.into(), tx))
            .await?;
        rx.await?
    }
    async fn discard_draft(&self, workspace: &str, id: &str, ack: (String, Value)) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Operation::DiscardDraft(
                workspace.into(),
                id.into(),
                ack,
                tx,
            ))
            .await?;
        rx.await?
    }
    async fn put_attachment(
        &self,
        chunks: &[Vec<u8>],
        mime: &str,
    ) -> Result<crate::domain::session::StoredAttachment> {
        self.save_attachment(chunks, mime).await
    }
    async fn snapshot_attachment(
        &self,
        path: &str,
        mime: &str,
    ) -> Result<crate::domain::session::StoredAttachment> {
        self.snapshot_file(path, mime).await
    }
    async fn read_attachment(
        &self,
        asset: &crate::domain::session::StoredAttachment,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>> {
        use tokio::io::{AsyncReadExt, AsyncSeekExt};
        let mut file = tokio::fs::File::open(&asset.path).await?;
        anyhow::ensure!(
            file.metadata().await?.len() == asset.total_bytes,
            "Attachment snapshot changed"
        );
        file.seek(std::io::SeekFrom::Start(offset)).await?;
        let mut bytes = vec![];
        file.take(limit as u64).read_to_end(&mut bytes).await?;
        Ok(bytes)
    }
    async fn load(&self, workspace: &str) -> Result<(Vec<Session>, BTreeMap<String, Value>)> {
        Store::load(self, workspace).await
    }
    async fn commit(
        &self,
        workspace: &str,
        session: Option<&mut Session>,
        ack: Option<(String, Value)>,
    ) -> Result<()> {
        Store::commit(self, workspace, session, ack).await
    }
}
