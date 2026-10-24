//! 操作账本（remote-exec.md §18.5、§19.7）：先记账再执行；完成的回复与流里没 ack 的事件留到客户端 ack。这里是内存
//! 实现；文件版（常驻执行器重启后按日志恢复）在 executor-host 的 `FileLedger`。
//!
//! 账本存编码好的帧（回复、事件），不认识它们的内容：补发时原样发出。

use std::collections::{BTreeMap, HashMap};
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;

/// 一次调用：按（客户端实例，调用 ID）区分，不同实例互不影响。
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct CallKey {
    pub instance: String,
    pub call: u64,
}

/// [`ExecLedger::begin`] 的结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Begun {
    /// 没见过：已经登记为进行中，调用方去执行。
    New,
    /// 正在执行（或流还开着）。
    Running,
    /// 已经完成：编码好的回复帧。
    Done(Bytes),
    /// 同一个调用 ID、不同的请求内容。
    Conflict,
    /// 执行器重启前没有终态：结果不明，不重跑。
    Interrupted,
}

/// 调用的种类（§19.7）：文件版账本只把有副作用的调用写进日志；只读调用重跑没有副作用，流的消费方按自己的游标重新
/// 订阅，所以它们只在内存里。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    Read,
    Mutation,
    Stream,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("exec ledger: {0}")]
pub struct LedgerError(pub String);

#[async_trait]
pub trait ExecLedger: Send + Sync {
    /// 先记账再执行：没见过就登记为进行中（带请求的指纹）；见过就给出它的状态。
    async fn begin(
        &self,
        key: &CallKey,
        fingerprint: &str,
        kind: CallKind,
    ) -> Result<Begun, LedgerError>;

    /// 一次调用完成：存下编码好的回复帧。
    async fn finish(&self, key: &CallKey, reply: Bytes) -> Result<(), LedgerError>;

    /// 流的一项（编码好的事件帧）。
    async fn append_event(&self, key: &CallKey, seq: u64, event: Bytes) -> Result<(), LedgerError>;

    /// 客户端 ack 到 `seq`（累计）：丢掉它及之前的事件。
    async fn ack_events(&self, key: &CallKey, seq: u64) -> Result<(), LedgerError>;

    /// `seq` 之后还留着的事件，按 `seq` 排序（补发）。
    async fn events_after(&self, key: &CallKey, seq: u64) -> Result<Vec<Bytes>, LedgerError>;

    /// 删掉一次调用（客户端 ack 了回复、流结束并 ack、或客户端关闭了流）。
    async fn forget(&self, key: &CallKey) -> Result<(), LedgerError>;

    /// 执行器启动时：没有终态的调用记成中断（之后 `begin` 给 [`Begun::Interrupted`]），返回这些调用。
    async fn recover(&self) -> Result<Vec<CallKey>, LedgerError>;

    /// 账本的身份（`welcome.ledger`，§19.8）：执行器重启后还是同一份账本时相同。内存账本随进程丢，没有身份。
    fn identity(&self) -> Option<String> {
        None
    }

    /// 一个客户端实例断开太久、不会再回来（§19.7）：删掉它只在内存里的记录。写进日志的回复按日志自己的保留规则。
    async fn forget_client(&self, _instance: &str) -> Result<(), LedgerError> {
        Ok(())
    }
}

enum State {
    Running,
    Done(Bytes),
}

struct Entry {
    fingerprint: String,
    state: State,
    events: BTreeMap<u64, Bytes>,
}

/// 内存里的账本：执行器进程活着时有效（R1 的 stdio 执行器断开就退出，重启即新实例）。
#[derive(Default)]
pub struct MemoryLedger {
    entries: Mutex<HashMap<CallKey, Entry>>,
}

impl MemoryLedger {
    fn entries(&self) -> std::sync::MutexGuard<'_, HashMap<CallKey, Entry>> {
        self.entries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// 账本里还有几次调用（测试看 ack 之后是否删掉）。
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }
}

#[async_trait]
impl ExecLedger for MemoryLedger {
    async fn begin(
        &self,
        key: &CallKey,
        fingerprint: &str,
        _kind: CallKind,
    ) -> Result<Begun, LedgerError> {
        let mut entries = self.entries();
        let Some(entry) = entries.get(key) else {
            entries.insert(
                key.clone(),
                Entry {
                    fingerprint: fingerprint.to_owned(),
                    state: State::Running,
                    events: BTreeMap::new(),
                },
            );
            return Ok(Begun::New);
        };
        if entry.fingerprint != fingerprint {
            return Ok(Begun::Conflict);
        }
        Ok(match &entry.state {
            State::Running => Begun::Running,
            State::Done(reply) => Begun::Done(reply.clone()),
        })
    }

    async fn finish(&self, key: &CallKey, reply: Bytes) -> Result<(), LedgerError> {
        if let Some(entry) = self.entries().get_mut(key) {
            entry.state = State::Done(reply);
        }
        Ok(())
    }

    async fn append_event(&self, key: &CallKey, seq: u64, event: Bytes) -> Result<(), LedgerError> {
        if let Some(entry) = self.entries().get_mut(key) {
            entry.events.insert(seq, event);
        }
        Ok(())
    }

    async fn ack_events(&self, key: &CallKey, seq: u64) -> Result<(), LedgerError> {
        if let Some(entry) = self.entries().get_mut(key) {
            entry.events.retain(|kept, _| *kept > seq);
        }
        Ok(())
    }

    async fn events_after(&self, key: &CallKey, seq: u64) -> Result<Vec<Bytes>, LedgerError> {
        Ok(self
            .entries()
            .get(key)
            .map(|entry| {
                entry
                    .events
                    .range(seq.saturating_add(1)..)
                    .map(|(_, event)| event.clone())
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn forget(&self, key: &CallKey) -> Result<(), LedgerError> {
        self.entries().remove(key);
        Ok(())
    }

    async fn recover(&self) -> Result<Vec<CallKey>, LedgerError> {
        // 内存账本随进程一起丢：启动时是空的，没有要恢复的。
        Ok(Vec::new())
    }

    async fn forget_client(&self, instance: &str) -> Result<(), LedgerError> {
        self.entries().retain(|key, _| key.instance != instance);
        Ok(())
    }
}
