//! `SessionStore` over the Node database (spec rust-m11-node-storage §5.1).
use super::node_store::{NodeStore, Request};
use crate::domain::session::{Session, StoredAttachment};
use crate::domain::session_listing::{ListParams, SessionListing};
use anyhow::{Result, bail};
use serde_json::Value;
use std::collections::BTreeMap;
use tokio::sync::oneshot;

impl NodeStore {
    /// Runs `query` on a request-scoped read-only connection after every
    /// usage fact sent before the call was written.
    async fn read_usage<T: Send + 'static>(
        &self,
        query: impl FnOnce(&rusqlite::Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Request::Barrier(tx)).await?;
        rx.await?;
        let path = self.path.clone();
        tokio::task::spawn_blocking(move || query(&super::usage_query::open(&path)?)).await?
    }
}

#[async_trait::async_trait]
impl crate::contract::SessionStore for NodeStore {
    fn node_journal(&self) -> bool {
        true
    }
    async fn load_index(&self, workspace: &str) -> Result<BTreeMap<String, Value>> {
        self.request(|reply| Request::Index(workspace.into(), reply))
            .await
    }
    async fn lookup_ack(&self, workspace: &str, key: &str) -> Result<Option<Value>> {
        self.lookup_ack_live(workspace, key, false).await
    }
    async fn lookup_ack_live(
        &self,
        _workspace: &str,
        key: &str,
        live: bool,
    ) -> Result<Option<Value>> {
        self.request(|reply| Request::Ack(key.into(), live, reply))
            .await
    }
    async fn list_sessions(
        &self,
        params: &ListParams,
        _owner: (&str, &str),
    ) -> Result<Vec<SessionListing>> {
        self.request(|reply| Request::List(params.clone(), reply))
            .await
    }
    async fn load_session(&self, workspace: &str, id: &str) -> Result<Option<Session>> {
        self.request(|reply| Request::Load(workspace.into(), id.into(), reply))
            .await
    }
    async fn project_settings(&self, workspace: &str) -> Result<BTreeMap<(String, String), Value>> {
        self.request(|reply| Request::Settings(workspace.into(), reply))
            .await
    }
    async fn save_project_setting(
        &self,
        workspace: &str,
        namespace: &str,
        key: &str,
        value: &Value,
    ) -> Result<()> {
        let key = (namespace.to_owned(), key.to_owned());
        self.request(|reply| Request::SaveSetting(workspace.into(), key, value.clone(), reply))
            .await
    }
    /// A draft never reached the database: its row is written with the first input.
    async fn discard_draft(
        &self,
        _workspace: &str,
        _id: &str,
        _ack: Option<(String, Value)>,
    ) -> Result<()> {
        Ok(())
    }
    async fn put_attachment(
        &self,
        session: &str,
        call: &str,
        chunks: &[Vec<u8>],
        mime: &str,
    ) -> Result<(String, StoredAttachment)> {
        self.attachments.put(session, call, chunks, mime).await
    }
    async fn local_attachment(
        &self,
        session: &str,
        index: usize,
        file: (&str, &str),
        mime: &str,
    ) -> Result<(String, StoredAttachment)> {
        self.attachments.local(session, index, file, mime).await
    }
    async fn attachment_of(&self, reference: &str) -> Result<Option<StoredAttachment>> {
        self.attachments.attachment_of(reference).await
    }
    async fn read_attachment(
        &self,
        asset: &StoredAttachment,
        offset: u64,
        limit: usize,
    ) -> Result<Vec<u8>> {
        super::input_attachments::read_attachment(asset, offset, limit).await
    }
    async fn record_usage(&self, fact: crate::domain::usage::Fact) {
        // worker 已停止时会话提交同样会失败；这里只丢弃观测数据。
        let _ = self.tx.send(Request::Usage(Box::new(fact))).await;
    }
    async fn app_usage(
        &self,
        since: i64,
        until: i64,
        offset: i64,
    ) -> Result<crate::domain::usage::AppRows> {
        self.read_usage(move |conn| super::usage_query::app(conn, "", since, until, offset))
            .await
    }
    async fn task_usage(&self, session_id: &str) -> Result<Vec<crate::domain::usage::TaskRow>> {
        let session = session_id.to_owned();
        self.read_usage(move |conn| super::usage_query::task(conn, "", &session))
            .await
    }
    async fn load(&self, _workspace: &str) -> Result<(Vec<Session>, BTreeMap<String, Value>)> {
        bail!("Whole-workspace loading is unavailable; sessions load on demand")
    }
    /// Node persists no command receipts (§7): only the session's journal
    /// writes are committed; a failure keeps them queued on the session.
    async fn commit(
        &self,
        _workspace: &str,
        session: Option<&mut Session>,
        _ack: Option<(String, Value)>,
    ) -> Result<()> {
        let Some(session) = session else {
            return Ok(());
        };
        let writes = session.node.take();
        if !writes.is_empty() {
            let (tx, rx) = oneshot::channel();
            if let Err(error) = self
                .tx
                .send(Request::Commit(session.id.clone(), writes, tx))
                .await
            {
                if let Request::Commit(_, writes, _) = error.0 {
                    session.node.pending = writes;
                }
                bail!("Storage worker stopped");
            }
            if let Err((error, writes)) = rx.await? {
                session.node.pending = writes;
                return Err(error);
            }
        }
        session.saved_inputs = session.history.inputs.len();
        session.saved_responses = session.history.responses.len();
        session.history_rewrite = false;
        session.saved_rows = session.rows.len();
        session.saved_messages = session.messages.len();
        session.pending_acks.clear();
        Ok(())
    }
}
