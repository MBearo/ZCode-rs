use super::storage::{Operation, Store};
use super::storage_settings::Request;
use crate::domain::session::Session;
use anyhow::Result;
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::oneshot;

impl Store {
    /// Waits until the worker handled every earlier operation.
    async fn barrier(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Operation::Barrier(tx)).await?;
        Ok(rx.await?)
    }

    /// Runs `query` on a request-scoped read-only connection off the worker.
    async fn read_usage<T: Send + 'static>(
        &self,
        query: impl FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.barrier().await?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || query(&super::usage_query::open(&path)?)).await?
    }
}

#[async_trait::async_trait]
impl crate::contract::SessionStore for Store {
    async fn record_usage(&self, fact: crate::domain::usage::Fact) {
        // worker 已停止时会话提交同样会失败；这里只丢弃观测数据。
        let _ = self.tx.send(Operation::Usage(Box::new(fact))).await;
    }
    async fn app_usage(
        &self,
        since: i64,
        until: i64,
        offset: i64,
    ) -> Result<crate::domain::usage::AppRows> {
        self.read_usage(move |conn| super::usage_query::app(conn, "rust_", since, until, offset))
            .await
    }
    async fn task_usage(&self, session_id: &str) -> Result<Vec<crate::domain::usage::TaskRow>> {
        let session = session_id.to_owned();
        self.read_usage(move |conn| super::usage_query::task(conn, "rust_", &session))
            .await
    }
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
    async fn discard_draft(
        &self,
        workspace: &str,
        id: &str,
        ack: Option<(String, Value)>,
    ) -> Result<()> {
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
        session: &str,
        call: &str,
        chunks: &[Vec<u8>],
        mime: &str,
    ) -> Result<(String, crate::domain::session::StoredAttachment)> {
        self.attachments().put(session, call, chunks, mime).await
    }
    async fn local_attachment(
        &self,
        session: &str,
        index: usize,
        file: (&str, &str),
        mime: &str,
    ) -> Result<(String, crate::domain::session::StoredAttachment)> {
        self.attachments().local(session, index, file, mime).await
    }
    async fn read_attachment(
        &self,
        asset: &crate::domain::session::StoredAttachment,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>> {
        super::input_attachments::read_attachment(asset, offset, limit).await
    }
    async fn project_settings(&self, workspace: &str) -> Result<BTreeMap<(String, String), Value>> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Operation::Settings(Request::Load(workspace.into(), tx)))
            .await?;
        rx.await?
    }
    async fn save_project_setting(
        &self,
        workspace: &str,
        namespace: &str,
        key: &str,
        value: &Value,
    ) -> Result<()> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Operation::Settings(Request::Save {
                workspace: workspace.into(),
                namespace: namespace.into(),
                key: key.into(),
                value: serde_json::to_string(value)?,
                reply,
            }))
            .await?;
        rx.await?
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
