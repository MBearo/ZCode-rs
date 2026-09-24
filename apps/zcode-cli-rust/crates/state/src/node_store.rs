//! The Node session database as the runtime's session store (spec
//! rust-m11-node-storage §5.1): one worker thread owns the only connection;
//! a commit applies the session's journal writes in one `begin immediate`
//! transaction; loads, lists and receipts read Node's records.
use crate::domain::node_journal::Write;
use crate::domain::session::Session;
use crate::domain::session_listing::{ListParams, SessionListing};
use crate::node::{acks, apply, artifacts, listing, load, open, resume, sessions, settings};
use anyhow::{Context, Result, bail};
use rusqlite::{Connection, TransactionBehavior};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use tokio::sync::{mpsc, oneshot};
use zcode_cli_domain::{node_ids, workspace_identity::parse_remote};

type Reply<T> = oneshot::Sender<Result<T>>;
pub(super) type Settings = BTreeMap<(String, String), Value>;
/// A failed commit hands its writes back so the session keeps them.
pub(super) type CommitReply = oneshot::Sender<Result<(), (anyhow::Error, Vec<Write>)>>;

pub(super) enum Request {
    Index(String, Reply<BTreeMap<String, Value>>),
    Ack(String, bool, Reply<Option<Value>>),
    List(ListParams, Reply<Vec<SessionListing>>),
    Load(String, String, Reply<Option<Session>>),
    Settings(String, Reply<Settings>),
    SaveSetting(String, (String, String), Value, Reply<()>),
    Commit(String, Vec<Write>, CommitReply),
    Usage(Box<crate::domain::usage::Fact>),
    /// Replies once every earlier request was handled.
    Barrier(oneshot::Sender<()>),
}

/// The session store over Node's `db.sqlite`.
#[derive(Clone)]
pub struct NodeStore {
    pub(super) tx: mpsc::Sender<Request>,
    /// Prompt attachments: Node data URL artifacts (spec §5.3).
    pub(super) attachments: super::input_attachments::Backing,
    /// The database file, for request-scoped read-only usage queries.
    pub(super) path: PathBuf,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The timeline host facts of a resumed session (its model selection,
/// execution state and directory).
fn host(r: &resume::Resume) -> zcode_cli_domain::node_journal::timeline::Host {
    let selection = r.model_selection.clone().unwrap_or(Value::Null);
    let text = |v: &Value| v.as_str().unwrap_or("").to_owned();
    zcode_cli_domain::node_journal::timeline::Host {
        session: r.session.id.clone(),
        agent: zcode_cli_domain::node_journal::records::AGENT.into(),
        provider: text(&selection["providerId"]),
        model: text(&selection["modelId"]),
        mode: r
            .execution
            .as_ref()
            .map_or("build".into(), |e| text(&e["mode"])),
        plan: r
            .execution
            .as_ref()
            .is_some_and(|e| e["planEnabled"] == true),
        cwd: r
            .session
            .path
            .clone()
            .unwrap_or_else(|| r.session.directory.clone()),
    }
}

/// The directory a workspace's project settings are keyed by.
fn directory(workspace: &str) -> &str {
    parse_remote(workspace).map_or(workspace, |r| r.path)
}

impl NodeStore {
    /// Opens and migrates the Node database; `artifacts` is Node's artifact
    /// root, `media_cache` Rust's own cache of prepared request media.
    pub async fn open(path: PathBuf, artifacts: PathBuf, media_cache: PathBuf) -> Result<Self> {
        let attachments = super::input_attachments::Backing {
            root: artifacts.clone(),
            cache: media_cache,
        };
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let (tx, rx) = mpsc::channel(64);
        let (ready_tx, ready_rx) = oneshot::channel();
        let database = path.clone();
        tokio::task::spawn_blocking(move || {
            match open::open(&path, open::MIGRATION_LOCK_WAIT, &mut |_| {}) {
                Ok(conn) => {
                    let _ = ready_tx.send(Ok(()));
                    Worker {
                        conn,
                        artifacts,
                        usage: crate::usage::Writer::new(""),
                    }
                    .run(rx);
                }
                Err(error) => {
                    let _ = ready_tx.send(Err(anyhow::Error::new(error)));
                }
            }
        });
        ready_rx.await.context("Storage worker stopped")??;
        Ok(Self {
            tx,
            attachments,
            path: database,
        })
    }

    pub(super) async fn request<T>(&self, make: impl FnOnce(Reply<T>) -> Request) -> Result<T> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(make(tx))
            .await
            .map_err(|_| anyhow::anyhow!("Storage worker stopped"))?;
        rx.await.context("Storage worker stopped")?
    }
}

struct Worker {
    conn: Connection,
    artifacts: PathBuf,
    usage: crate::usage::Writer,
}

impl Worker {
    fn run(mut self, mut rx: mpsc::Receiver<Request>) {
        while let Some(request) = rx.blocking_recv() {
            match request {
                Request::Index(workspace, reply) => {
                    let _ = reply.send(self.index(&workspace));
                }
                Request::Ack(key, live, reply) => {
                    let _ = reply.send(self.ack(&key, live));
                }
                Request::List(params, reply) => {
                    let _ = reply.send(self.list(&params));
                }
                Request::Load(workspace, id, reply) => {
                    let _ = reply.send(self.load(&workspace, &id));
                }
                Request::Settings(workspace, reply) => {
                    let _ = reply.send(self.settings(&workspace));
                }
                Request::SaveSetting(workspace, (namespace, key), value, reply) => {
                    let _ = reply.send(self.save_setting(&workspace, (&namespace, &key), &value));
                }
                Request::Commit(session, writes, reply) => {
                    let _ = reply.send(self.commit(&session, &writes).map_err(|e| (e, writes)));
                }
                Request::Usage(fact) => {
                    // 用量是观测数据：写入失败只记日志，不影响会话提交。
                    if let Err(error) = self.usage.record(&self.conn, &fact, now_ms() as u64) {
                        tracing::warn!(target: "zcode::storage", event = "usage.write.failed",
                            error = %error, "Usage fact write failed");
                    }
                }
                Request::Barrier(reply) => {
                    let _ = reply.send(());
                }
            }
        }
    }

    fn commit(&mut self, session: &str, writes: &[Write]) -> Result<()> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        apply::apply(&tx, session, writes)?;
        tx.commit()?;
        Ok(())
    }

    fn index(&self, workspace: &str) -> Result<BTreeMap<String, Value>> {
        Ok(listing::stored_summaries(&self.conn, workspace)?
            .into_iter()
            .map(|summary| {
                (
                    summary["sessionId"].as_str().unwrap_or("").to_owned(),
                    summary,
                )
            })
            .collect())
    }

    fn ack(&self, key: &str, live: bool) -> Result<Option<Value>> {
        let (session, command): (Option<String>, String) = serde_json::from_str(key)?;
        match session {
            None => acks::lookup_create(&self.conn, &command, now_ms()),
            Some(session) => acks::lookup(&self.conn, (&session, live), &command, now_ms()),
        }
    }

    fn list(&self, params: &ListParams) -> Result<Vec<SessionListing>> {
        Ok(listing::rows(&self.conn, params)?
            .into_iter()
            .map(|row| {
                // Node `buildWorkspaceRef({ workspacePath: session.path ?? session.directory })`.
                let path = row.path.clone().unwrap_or_else(|| row.directory.clone());
                SessionListing {
                    id: row.id,
                    workspace: path.clone(),
                    workspace_path: Some(path),
                    workspace_directory: Some(row.directory),
                    prompt_path: None,
                    trace_id: row.trace_id,
                    task_type: row.task_type,
                    title: row.title,
                    title_source: row.title_source,
                    parent_id: row.parent_id,
                    created_at: row.time_created.max(0) as u64,
                    updated_at: row.time_updated.max(0) as u64,
                    archived_at: row.time_archived.map(|t| t.max(0) as u64),
                }
            })
            .collect())
    }

    /// Node `resumeFromStore` for this workspace's session; queued inputs do
    /// not survive a restart (`discarded/session_resumed`).
    fn load(&self, workspace: &str, id: &str) -> Result<Option<Session>> {
        let Some(row) = sessions::get(&self.conn, id)? else {
            return Ok(None);
        };
        if listing::workspace_key(&row) != workspace {
            return Ok(None);
        }
        let root: &Path = &self.artifacts;
        let reader = |uri: &str| artifacts::read(root, uri);
        let Some(mut resumed) = resume::resume(&self.conn, id, &reader, None)? else {
            return Ok(None);
        };
        let now = now_ms();
        // Node resume：停在进行中的压缩时间线收口为 completed / interrupted，再按新记录投影。
        if crate::node::compact::recover(&self.conn, &host(&resumed), now)? > 0 {
            resumed = resume::resume(&self.conn, id, &reader, None)?.context("Session vanished")?;
        }
        for input in crate::node::inputs::list(&self.conn, id, Some("admitted"))? {
            crate::node::inputs::settle(
                &self.conn,
                &input.id,
                id,
                "discarded",
                Some("session_resumed"),
                now,
            )?;
        }
        Ok(Some(load::session(workspace, resumed, crate::id())))
    }

    fn settings(&self, workspace: &str) -> Result<Settings> {
        let dir = directory(workspace);
        let mut out = Settings::new();
        let permission = |key: &str| ("permission".to_owned(), key.to_owned());
        if let Some(ruleset) = settings::project_permission(&self.conn, &node_ids::project_id(dir))?
        {
            out.insert(permission("ruleset"), ruleset);
        }
        let mode = settings::project_permission_mode(&self.conn, &node_ids::app_project_id(dir))?;
        if let Some(mode) = mode {
            out.insert(permission("mode"), json!({"mode": mode}));
        }
        Ok(out)
    }

    /// Node `saveProjectPermission` (core project id) and
    /// `saveProjectPermissionMode` (bootstrap project id).
    fn save_setting(&self, workspace: &str, key: (&str, &str), value: &Value) -> Result<()> {
        let dir = directory(workspace);
        match key {
            ("permission", "ruleset") => settings::save_project_permission(
                &self.conn,
                &node_ids::project_id(dir),
                value,
                now_ms(),
            ),
            ("permission", "mode") => settings::save_project_permission_mode(
                &self.conn,
                &node_ids::app_project_id(dir),
                value["mode"].as_str().context("Invalid permission mode")?,
                now_ms(),
            ),
            _ => bail!("Unsupported project setting"),
        }
    }
}

#[cfg(test)]
#[path = "node_store_tests.rs"]
mod tests;
