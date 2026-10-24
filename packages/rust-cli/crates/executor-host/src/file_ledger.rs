//! 文件版账本（docs/specs/rust-cli/remote-exec.md §19.7）：有副作用的调用的登记与回复写进 `ledger/journal`，常驻
//! 执行器重启后按日志恢复——重启前没有终态的记成中断（不重跑），完成的回复仍能取回。只读调用与流只在内存里（交给
//! [`MemoryLedger`]）：只读调用重跑没有副作用，流的消费方按自己的游标重新订阅。
//!
//! 日志的状态只归一个写者任务（actor）所有：每次操作是一条消息，写者按顺序改状态、追加日志，锁不跨 await。
//!
//! 保留与清理（§19.7 的表）：完成而没 ack 的回复 24 小时后变成墓碑；墓碑 7 天后删掉；日志超过 64 MiB 时从最早完成
//! 的回复起变成墓碑，降到 48 MiB 以下；日志超过 4 MiB 且超过活着的记录的 2 倍时压缩。时间取注入的 `Clock`。

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use rust_cli_base::OwnedTask;
use rust_cli_exec_server::{Begun, CallKey, CallKind, ExecLedger, LedgerError, MemoryLedger};
use rust_cli_kernel_api::clock::Clock;
use tokio::sync::{mpsc, oneshot};

use crate::journal::{Entry, Record, State, Tag, apply, records_of, replay};

const JOURNAL: &str = "journal";
const IDENTITY: &str = "id";
/// 写者的信箱：满了调用方就等（背压）。
const MAILBOX: usize = 64;

/// 保留与清理的数值（§19.7；测试换成小的）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retention {
    /// 完成而没 ack 的回复留多久。
    pub done: Duration,
    /// 墓碑留多久。
    pub tomb: Duration,
    /// 日志的大小上限。
    pub max_bytes: u64,
    /// 超过上限后降到这里以下。
    pub target_bytes: u64,
    /// 日志至少这么大才考虑压缩。
    pub compact_min: u64,
}

impl Default for Retention {
    fn default() -> Self {
        Self {
            done: Duration::from_secs(24 * 60 * 60),
            tomb: Duration::from_secs(7 * 24 * 60 * 60),
            max_bytes: 64 * 1024 * 1024,
            target_bytes: 48 * 1024 * 1024,
            compact_min: 4 * 1024 * 1024,
        }
    }
}

type Reply<T> = oneshot::Sender<Result<T, LedgerError>>;

/// 给写者的消息。
enum Command {
    Begin(CallKey, String, Reply<Begun>),
    /// 回 true：这次调用在日志里（已写下完成）；false：不在，调用方交给内存账本。
    Finish(CallKey, Bytes, Reply<bool>),
    Forget(CallKey, Reply<()>),
    Recover(Reply<Vec<CallKey>>),
    Sweep(Reply<(usize, usize)>),
    Size(Reply<(usize, u64)>),
}

/// 写者独有的状态。
struct Journal {
    path: PathBuf,
    clock: Arc<dyn Clock>,
    retention: Retention,
    entries: HashMap<CallKey, Entry>,
    /// 日志文件现在的字节数。
    bytes: u64,
}

/// 文件版账本。
pub struct FileLedger {
    identity: String,
    memory: MemoryLedger,
    mailbox: mpsc::Sender<Command>,
    _writer: OwnedTask<()>,
}

fn error(context: &str, error: &std::io::Error) -> LedgerError {
    LedgerError(format!("{context}: {error}"))
}

fn closed() -> LedgerError {
    LedgerError("the ledger writer has stopped".to_owned())
}

fn private() -> rust_cli_fs::WriteOptions {
    rust_cli_fs::WriteOptions {
        mode: rust_cli_fs::FileMode::Private,
        temp_name: rust_cli_fs::TempName::Hidden,
        parent: rust_cli_fs::ParentDir::Existing,
        rename_retry: &[],
    }
}

impl FileLedger {
    /// 打开（没有就建）`dir` 下的账本：读身份与日志，重放。恢复（重启前没有终态的记成中断）在 [`ExecLedger::recover`]。
    pub async fn open(
        dir: &Path,
        clock: Arc<dyn Clock>,
        retention: Retention,
    ) -> Result<Self, LedgerError> {
        rust_cli_fs::create_private_dir_all(dir)
            .await
            .map_err(|e| error("creating the ledger directory", &e))?;
        let identity = identity(&dir.join(IDENTITY)).await?;
        let path = dir.join(JOURNAL);
        let text = rust_cli_fs::read_optional(&path)
            .await
            .map_err(|e| error("reading the ledger", &e))?
            .unwrap_or_default();
        let bytes = text.len() as u64;
        let (entries, skipped) = replay(&String::from_utf8_lossy(&text));
        if skipped > 0 {
            tracing::warn!(target: "executor_host::ledger", skipped, "unreadable ledger records were skipped");
        }
        let journal = Journal {
            path,
            clock,
            retention,
            entries,
            bytes,
        };
        let (mailbox, inbox) = mpsc::channel(MAILBOX);
        Ok(Self {
            identity,
            memory: MemoryLedger::default(),
            mailbox,
            _writer: rust_cli_base::owned(journal.serve(inbox)),
        })
    }

    async fn ask<T>(&self, command: impl FnOnce(Reply<T>) -> Command) -> Result<T, LedgerError> {
        let (tx, rx) = oneshot::channel();
        self.mailbox.send(command(tx)).await.map_err(|_| closed())?;
        rx.await.map_err(|_| closed())?
    }

    /// 按保留规则清理（启动时、每 10 分钟、超过大小上限时；§19.7）。返回变成墓碑与删掉的项数。
    pub async fn sweep(&self) -> Result<(usize, usize), LedgerError> {
        self.ask(Command::Sweep).await
    }

    /// 日志里的项数（测试看清理的结果）。
    pub async fn durable_len(&self) -> usize {
        self.ask(Command::Size).await.map_or(0, |(len, _)| len)
    }

    /// 日志文件的字节数（测试看压缩的结果）。
    pub async fn journal_bytes(&self) -> u64 {
        self.ask(Command::Size).await.map_or(0, |(_, bytes)| bytes)
    }
}

impl Journal {
    async fn serve(mut self, mut inbox: mpsc::Receiver<Command>) {
        while let Some(command) = inbox.recv().await {
            match command {
                Command::Begin(key, fingerprint, reply) => {
                    let _ = reply.send(self.begin(key, fingerprint).await);
                }
                Command::Finish(key, bytes, reply) => {
                    let _ = reply.send(self.finish(&key, &bytes).await);
                }
                Command::Forget(key, reply) => {
                    let _ = reply.send(self.forget(&key).await);
                }
                Command::Recover(reply) => {
                    let _ = reply.send(self.recover().await);
                }
                Command::Sweep(reply) => {
                    let _ = reply.send(self.sweep().await);
                }
                Command::Size(reply) => {
                    let _ = reply.send(Ok((self.entries.len(), self.bytes)));
                }
            }
        }
    }

    fn live(&self) -> u64 {
        self.entries.values().map(Entry::weight).sum()
    }

    async fn append(&mut self, record: Record) -> Result<(), LedgerError> {
        let line = record.line();
        rust_cli_fs::append(&self.path, line.as_bytes())
            .await
            .map_err(|e| error("writing the ledger", &e))?;
        self.bytes += line.len() as u64;
        apply(&mut self.entries, record);
        Ok(())
    }

    async fn begin(&mut self, key: CallKey, fingerprint: String) -> Result<Begun, LedgerError> {
        if let Some(entry) = self.entries.get(&key) {
            if entry.fingerprint != fingerprint {
                return Ok(Begun::Conflict);
            }
            return Ok(match &entry.state {
                State::Running => Begun::Running,
                State::Done(reply) => Begun::Done(reply.clone()),
                State::Tomb => Begun::Interrupted,
            });
        }
        // 先记账再执行：这一行写下之后才回 New。
        let now = self.clock.now_ms();
        self.append(Record::begin(&key, &fingerprint, now)).await?;
        Ok(Begun::New)
    }

    async fn finish(&mut self, key: &CallKey, reply: &[u8]) -> Result<bool, LedgerError> {
        if !self.entries.contains_key(key) {
            return Ok(false);
        }
        let now = self.clock.now_ms();
        self.append(Record::finish(key, reply, now)).await?;
        if self.live() > self.retention.max_bytes {
            self.sweep().await?;
        }
        Ok(true)
    }

    async fn forget(&mut self, key: &CallKey) -> Result<(), LedgerError> {
        if self.entries.contains_key(key) {
            let now = self.clock.now_ms();
            self.append(Record::mark(Tag::Forget, key, now)).await?;
        }
        Ok(())
    }

    async fn recover(&mut self) -> Result<Vec<CallKey>, LedgerError> {
        let running: Vec<CallKey> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.state == State::Running)
            .map(|(key, _)| key.clone())
            .collect();
        let now = self.clock.now_ms();
        for key in &running {
            self.append(Record::mark(Tag::Interrupted, key, now))
                .await?;
        }
        self.sweep().await?;
        Ok(running)
    }

    async fn sweep(&mut self) -> Result<(usize, usize), LedgerError> {
        let now = self.clock.now_ms();
        let ms = |duration: Duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX);
        let mut expired = Vec::new();
        let mut removed = Vec::new();
        for (key, entry) in &self.entries {
            let age = now.saturating_sub(entry.at);
            match entry.state {
                State::Done(_) if age >= ms(self.retention.done) => expired.push(key.clone()),
                State::Tomb if age >= ms(self.retention.tomb) => removed.push(key.clone()),
                State::Done(_) | State::Tomb | State::Running => {}
            }
        }
        // 超过大小上限：从最早完成的回复起变成墓碑。
        let mut live = self.live();
        if live > self.retention.max_bytes {
            let mut done: Vec<(u64, CallKey, u64)> = self
                .entries
                .iter()
                .filter(|(key, entry)| {
                    matches!(entry.state, State::Done(_)) && !expired.contains(key)
                })
                .map(|(key, entry)| (entry.at, key.clone(), entry.weight()))
                .collect();
            done.sort();
            for (_, key, weight) in done {
                if live <= self.retention.target_bytes {
                    break;
                }
                live = live.saturating_sub(weight);
                expired.push(key);
            }
        }
        for key in &expired {
            self.append(Record::mark(Tag::Expired, key, now)).await?;
        }
        for key in &removed {
            self.append(Record::mark(Tag::Forget, key, now)).await?;
        }
        self.compact_if_needed().await?;
        Ok((expired.len(), removed.len()))
    }

    /// 日志超过 `compact_min` 且超过活着的记录的 2 倍时，写成只有活着的记录的新文件（原子替换）。
    async fn compact_if_needed(&mut self) -> Result<(), LedgerError> {
        if self.bytes < self.retention.compact_min || self.bytes < self.live() * 2 {
            return Ok(());
        }
        let mut text = String::new();
        for (key, entry) in &self.entries {
            for record in records_of(key, entry) {
                text.push_str(&record.line());
            }
        }
        rust_cli_fs::atomic_write(&self.path, text.as_bytes(), private())
            .await
            .map_err(|e| error("compacting the ledger", &e))?;
        tracing::info!(target: "executor_host::ledger", before = self.bytes, after = text.len(), "ledger compacted");
        self.bytes = text.len() as u64;
        Ok(())
    }
}

/// 账本的身份：第一次建账本时生成，之后不变（§19.8）。
async fn identity(path: &Path) -> Result<String, LedgerError> {
    if let Some(bytes) = rust_cli_fs::read_optional(path)
        .await
        .map_err(|e| error("reading the ledger identity", &e))?
    {
        let text = String::from_utf8_lossy(&bytes).trim().to_owned();
        if !text.is_empty() {
            return Ok(text);
        }
    }
    let fresh = format!(
        "{:016x}{:016x}",
        rust_cli_base::random::u64(),
        rust_cli_base::random::u64()
    );
    rust_cli_fs::atomic_write(path, fresh.as_bytes(), private())
        .await
        .map_err(|e| error("writing the ledger identity", &e))?;
    Ok(fresh)
}

#[async_trait]
impl ExecLedger for FileLedger {
    async fn begin(
        &self,
        key: &CallKey,
        fingerprint: &str,
        kind: CallKind,
    ) -> Result<Begun, LedgerError> {
        if kind != CallKind::Mutation {
            return self.memory.begin(key, fingerprint, kind).await;
        }
        let (key, fingerprint) = (key.clone(), fingerprint.to_owned());
        self.ask(|reply| Command::Begin(key, fingerprint, reply))
            .await
    }

    async fn finish(&self, key: &CallKey, reply: Bytes) -> Result<(), LedgerError> {
        let durable = {
            let (key, reply) = (key.clone(), reply.clone());
            self.ask(|done| Command::Finish(key, reply, done)).await?
        };
        if durable {
            return Ok(());
        }
        self.memory.finish(key, reply).await
    }

    async fn append_event(&self, key: &CallKey, seq: u64, event: Bytes) -> Result<(), LedgerError> {
        self.memory.append_event(key, seq, event).await
    }

    async fn ack_events(&self, key: &CallKey, seq: u64) -> Result<(), LedgerError> {
        self.memory.ack_events(key, seq).await
    }

    async fn events_after(&self, key: &CallKey, seq: u64) -> Result<Vec<Bytes>, LedgerError> {
        self.memory.events_after(key, seq).await
    }

    async fn forget(&self, key: &CallKey) -> Result<(), LedgerError> {
        let owned = key.clone();
        self.ask(|done| Command::Forget(owned, done)).await?;
        self.memory.forget(key).await
    }

    async fn recover(&self) -> Result<Vec<CallKey>, LedgerError> {
        self.ask(Command::Recover).await
    }

    fn identity(&self) -> Option<String> {
        Some(self.identity.clone())
    }

    async fn forget_client(&self, instance: &str) -> Result<(), LedgerError> {
        // 只删内存里的；日志里的回复按自己的保留规则（变成墓碑，同一个调用 ID 再来不会重跑）。
        self.memory.forget_client(instance).await
    }
}

#[cfg(test)]
#[path = "file_ledger_tests.rs"]
mod tests;
