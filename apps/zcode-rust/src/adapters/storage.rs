use crate::domain::session::Session;
use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use std::{collections::BTreeMap, path::PathBuf};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

type StoredWorkspace = (Vec<Session>, BTreeMap<String, Value>);
pub(super) enum Operation {
    Index(String, oneshot::Sender<Result<BTreeMap<String, Value>>>),
    Ack(String, String, oneshot::Sender<Result<Option<Value>>>),
    List(
        crate::domain::session_listing::ListParams,
        (String, String),
        oneshot::Sender<Result<Vec<crate::domain::session_listing::SessionListing>>>,
    ),
    Import(
        super::legacy_storage::ImportRequest,
        oneshot::Sender<Result<()>>,
    ),
    Load(String, oneshot::Sender<Result<StoredWorkspace>>),
    LoadSession(String, String, oneshot::Sender<Result<Option<Session>>>),
    DiscardDraft(String, String, (String, Value), oneshot::Sender<Result<()>>),
    Commit(
        String,
        Option<Box<SessionWrite>>,
        Option<(String, Value)>,
        oneshot::Sender<Result<()>>,
    ),
}

#[derive(Clone)]
pub struct Store {
    pub(super) tx: mpsc::Sender<Operation>,
    pub(super) attachment_root: PathBuf,
}

impl Store {
    pub async fn lock_workspace(dir: PathBuf, workspace: String) -> Result<std::fs::File> {
        use sha2::{Digest, Sha256};
        tokio::task::spawn_blocking(move || {
            let key = format!("{:x}", Sha256::digest(workspace.as_bytes()));
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(dir.join(format!("workspace-{key}.lock")))?;
            // SQLite 的写锁不能阻止第二个 actor 先读取旧状态并执行恢复，必须先锁 owner。
            file.try_lock()
                .context("Workspace runtime is already owned or cannot be locked")?;
            Ok(file)
        })
        .await?
    }
    pub async fn open(path: PathBuf) -> Result<Self> {
        let attachment_root = path
            .parent()
            .context("Storage directory missing")?
            .join("attachments");
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let (tx, mut rx) = mpsc::channel(64);
        let (ready_tx, ready_rx) = oneshot::channel();
        tokio::task::spawn_blocking(move || {
            let connection = (|| -> Result<Connection> {
                let conn = Connection::open(path)?;
                conn.busy_timeout(std::time::Duration::from_secs(5))?;
                conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
                    CREATE TABLE IF NOT EXISTS rust_session(workspace TEXT NOT NULL,id TEXT NOT NULL,body TEXT NOT NULL,PRIMARY KEY(workspace,id));
                    CREATE TABLE IF NOT EXISTS rust_history(workspace TEXT NOT NULL,session TEXT NOT NULL,kind TEXT NOT NULL,ordinal INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(workspace,session,kind,ordinal));
                    CREATE TABLE IF NOT EXISTS rust_started(workspace TEXT NOT NULL,session TEXT NOT NULL,command TEXT NOT NULL,PRIMARY KEY(workspace,session,command));
                    CREATE TABLE IF NOT EXISTS rust_command(workspace TEXT NOT NULL,key TEXT NOT NULL,ack TEXT NOT NULL,PRIMARY KEY(workspace,key));
                    CREATE TABLE IF NOT EXISTS rust_row(workspace TEXT NOT NULL,session TEXT NOT NULL,ordinal INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(workspace,session,ordinal));
                    CREATE TABLE IF NOT EXISTS rust_message(workspace TEXT NOT NULL,session TEXT NOT NULL,ordinal INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(workspace,session,ordinal));")?;
                super::storage_listing::prepare(&conn)?;
                conn.execute_batch("CREATE INDEX IF NOT EXISTS rust_row_command ON rust_row(workspace,session,CASE WHEN json_valid(body) THEN json_extract(body,'$.sourceCommandId') END);")?;
                Ok(conn)
            })();
            let mut conn = match connection {
                Ok(c) => {
                    let _ = ready_tx.send(Ok(()));
                    c
                }
                Err(e) => {
                    let _ = ready_tx.send(Err(e));
                    return;
                }
            };
            while let Some(op) = rx.blocking_recv() {
                match op {
                    Operation::Index(workspace, reply) => {
                        let _ = reply.send(super::storage_index::index(&conn, &workspace));
                    }
                    Operation::Ack(workspace, key, reply) => {
                        let _ = reply.send(super::storage_index::ack(&mut conn, &workspace, &key));
                    }
                    Operation::List(params, owner, reply) => {
                        let _ = reply.send(super::storage_listing::list(
                            &conn,
                            &params,
                            (&owner.0, &owner.1),
                        ));
                    }
                    Operation::Import(request, reply) => {
                        let _ = reply.send(super::legacy_storage::import(&mut conn, request));
                    }
                    Operation::Load(workspace, reply) => {
                        let _ = reply.send(super::storage_read::load(&conn, &workspace));
                    }
                    Operation::LoadSession(workspace, id, reply) => {
                        let _ = reply.send(load_session(&conn, &workspace, &id));
                    }
                    Operation::DiscardDraft(workspace, id, ack, reply) => {
                        let _ = reply.send(discard_draft(&mut conn, &workspace, &id, ack));
                    }
                    Operation::Commit(workspace, session, ack, reply) => {
                        let _ = reply.send(commit(&mut conn, &workspace, session.map(|s| *s), ack));
                    }
                }
            }
        });
        ready_rx.await.context("Storage worker stopped")??;
        Ok(Self {
            tx,
            attachment_root,
        })
    }
    pub async fn load(&self, workspace: &str) -> Result<(Vec<Session>, BTreeMap<String, Value>)> {
        let (tx, rx) = oneshot::channel();
        self.tx.send(Operation::Load(workspace.into(), tx)).await?;
        rx.await?
    }
    pub async fn import_ts(
        &self,
        source: PathBuf,
        workspace: String,
        cwd: String,
        dir: PathBuf,
        artifacts: PathBuf,
        cancel: CancellationToken,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Operation::Import(
                super::legacy_storage::ImportRequest {
                    source,
                    workspace,
                    cwd,
                    dir,
                    artifacts,
                    cancel,
                },
                tx,
            ))
            .await?;
        rx.await?
    }
    pub async fn commit(
        &self,
        workspace: &str,
        session: Option<&mut Session>,
        ack: Option<(String, Value)>,
    ) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Operation::Commit(
                workspace.into(),
                session
                    .as_deref()
                    .map(SessionWrite::new)
                    .transpose()?
                    .map(Box::new),
                ack,
                tx,
            ))
            .await?;
        rx.await??;
        if let Some(session) = session {
            session.saved_inputs = session.history.inputs.len();
            session.saved_responses = session.history.responses.len();
            session.history_rewrite = false;
            session.saved_rows = session.rows.len();
            session.saved_messages = session.messages.len();
            session.pending_acks.clear();
        }
        Ok(())
    }
}
pub(super) fn commit(
    conn: &mut Connection,
    workspace: &str,
    session: Option<SessionWrite>,
    ack: Option<(String, Value)>,
) -> Result<()> {
    let tx = conn.transaction()?;
    write(&tx, workspace, session, ack)?;
    tx.commit()?;
    Ok(())
}
pub(super) fn write(
    tx: &Connection,
    workspace: &str,
    session: Option<SessionWrite>,
    ack: Option<(String, Value)>,
) -> Result<()> {
    if let Some(session) = session {
        // 截断前保存原 command 的已启动事实，旧 ACK 不能因为显示行消失被误判为丢弃。
        if session.rewrite {
            tx.execute("INSERT OR IGNORE INTO rust_started SELECT workspace,session,json_extract(body,'$.sourceCommandId') FROM rust_row WHERE workspace=?1 AND session=?2 AND json_extract(body,'$.sourceCommandId') IS NOT NULL",params![workspace,session.id])?;
            for table in ["rust_row", "rust_message"] {
                tx.execute(
                    &format!("DELETE FROM {table} WHERE workspace=?1 AND session=?2"),
                    params![workspace, session.id],
                )?;
            }
        }
        for command in &session.started {
            tx.execute(
                "INSERT OR IGNORE INTO rust_started VALUES(?1,?2,?3)",
                params![workspace, session.id, command],
            )?;
        }
        tx.execute("INSERT INTO rust_session VALUES(?1,?2,?3) ON CONFLICT(workspace,id) DO UPDATE SET body=excluded.body",params![workspace,session.id,session.metadata])?;
        session.history.save(tx, workspace, &session.id)?;
        for (table, start, values) in [
            ("rust_row", session.row_start, session.rows),
            ("rust_message", session.message_start, session.messages),
        ] {
            let mut statement = tx.prepare_cached(&format!("INSERT INTO {table} VALUES(?1,?2,?3,?4) ON CONFLICT(workspace,session,ordinal) DO UPDATE SET body=excluded.body WHERE body IS NOT excluded.body"))?;
            for (offset, value) in values.into_iter().enumerate() {
                statement.execute(params![
                    workspace,
                    session.id,
                    (start + offset) as i64,
                    value
                ])?;
            }
        }
        for (key, ack) in session.pending_acks {
            tx.execute("INSERT INTO rust_command VALUES(?1,?2,?3) ON CONFLICT(workspace,key) DO UPDATE SET ack=excluded.ack", params![workspace,key,serde_json::to_string(&ack)?])?;
        }
        if let Some((key, ack)) = session.creation_ack {
            tx.execute(
                "INSERT INTO rust_command VALUES(?1,?2,?3) ON CONFLICT(workspace,key) DO NOTHING",
                params![workspace, key, serde_json::to_string(&ack)?],
            )?;
        }
    }
    if let Some((key, ack)) = ack {
        tx.execute("INSERT INTO rust_command VALUES(?1,?2,?3) ON CONFLICT(workspace,key) DO UPDATE SET ack=excluded.ack",params![workspace,key,serde_json::to_string(&ack)?])?;
    }
    Ok(())
}

pub(super) struct SessionWrite {
    history: super::storage_history::Write,
    rewrite: bool,
    id: String,
    metadata: String,
    row_start: usize,
    rows: Vec<String>,
    started: Vec<String>,
    message_start: usize,
    messages: Vec<String>,
    creation_ack: Option<(String, Value)>,
    pending_acks: BTreeMap<String, Value>,
}
impl SessionWrite {
    pub(super) fn new(session: &Session) -> Result<Self> {
        let row_start = if session.history_rewrite {
            0
        } else {
            session.saved_rows.min(session.current_rows_start())
        };
        let message_start = if session.history_rewrite {
            0
        } else {
            session.saved_messages
        };
        Ok(Self {
            history: super::storage_history::Write::new(session)?,
            rewrite: session.history_rewrite,
            id: session.id.clone(),
            metadata: serde_json::to_string(session)?,
            row_start,
            rows: session.rows[row_start..]
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<_, _>>()?,
            started: session.rows[row_start..]
                .iter()
                .filter_map(|r| r["sourceCommandId"].as_str().map(str::to_owned))
                .collect(),
            message_start,
            messages: session.messages[message_start..]
                .iter()
                .map(serde_json::to_string)
                .collect::<Result<_, _>>()?,
            creation_ack: session.creation_ack.clone(),
            pending_acks: session.pending_acks.clone(),
        })
    }
}
pub(super) fn load_items(
    conn: &Connection,
    table: &str,
    workspace: &str,
    id: &str,
) -> Result<Vec<Value>> {
    let mut query = conn.prepare_cached(&format!(
        "SELECT body FROM {table} WHERE workspace=?1 AND session=?2 ORDER BY ordinal"
    ))?;
    query
        .query_map(params![workspace, id], |row| row.get::<_, String>(0))?
        .map(|row| Ok(serde_json::from_str(&row?)?))
        .collect()
}

fn load_session(conn: &Connection, workspace: &str, id: &str) -> Result<Option<Session>> {
    let body: Option<String> = conn
        .query_row(
            "SELECT body FROM rust_session WHERE workspace=?1 AND id=?2",
            params![workspace, id],
            |row| row.get(0),
        )
        .optional()?;
    let Some(body) = body else { return Ok(None) };
    let mut session: Session = serde_json::from_str(&body)?;
    if session.rows.is_empty() {
        session.rows = load_items(conn, "rust_row", workspace, id)?;
        session.messages = load_items(conn, "rust_message", workspace, id)?;
        session.saved_rows = session.rows.len();
        session.saved_messages = session.messages.len();
    }
    if session.history.inputs.is_empty() && session.history.responses.is_empty() {
        session.history = super::storage_history::load(conn, workspace, id)?;
        session.saved_inputs = session.history.inputs.len();
        session.saved_responses = session.history.responses.len();
    }
    Ok(Some(session))
}

fn discard_draft(
    conn: &mut Connection,
    workspace: &str,
    id: &str,
    ack: (String, Value),
) -> Result<()> {
    let tx = conn.transaction()?;
    // 关闭草稿不是真删历史；存储边界再次核查，避免 future caller 用过期 draft 状态误删首发。
    let Some(session) = load_session(&tx, workspace, id)? else {
        // 预热草稿只活在内存；关闭不应为每次界面切换增加永久 ACK 或 WAL 写入。
        return Ok(());
    };
    anyhow::ensure!(
        session.rows.is_empty() && session.messages.is_empty(),
        "Cannot discard a persisted conversation as draft"
    );
    tx.execute(
        "DELETE FROM rust_session WHERE workspace=?1 AND id=?2",
        params![workspace, id],
    )?;
    tx.execute("INSERT INTO rust_command VALUES(?1,?2,?3) ON CONFLICT(workspace,key) DO UPDATE SET ack=excluded.ack", params![workspace,ack.0,serde_json::to_string(&ack.1)?])?;
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
#[path = "storage_tests.rs"]
mod tests;
