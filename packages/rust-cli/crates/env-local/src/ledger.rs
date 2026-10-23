//! 写操作账本：同一个 operationId 只执行一次（docs/specs/rust-cli/README.md §6 规则 3）。
//!
//! 重放（同一个 ID、同样的参数）返回第一次的回执，不再写；同一个 ID 换了参数按冲突拒绝。
//! 账本属于这个执行环境实例，只保留最近的 [`CAPACITY`] 条；进程重启后由内核按落库的意图判断
//! （本地执行环境的 generation 随进程变化）。

use std::collections::{HashMap, VecDeque};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rust_cli_kernel_api::env::{EnvError, WritePrecondition};
use rust_cli_kernel_api::env_files::{FileWritten, PutFile};
use rust_cli_kernel_api::ids::OperationId;

const CAPACITY: usize = 1024;

#[derive(Default)]
struct Entries {
    done: HashMap<OperationId, (u64, FileWritten)>,
    order: VecDeque<OperationId>,
}

#[derive(Default)]
pub(crate) struct Ledger(Mutex<Entries>);

impl Ledger {
    fn entries(&self) -> MutexGuard<'_, Entries> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn fingerprint(request: &PutFile) -> u64 {
        let mut hasher = DefaultHasher::new();
        request.path.as_str().hash(&mut hasher);
        request.bytes.hash(&mut hasher);
        request.create_parents.hash(&mut hasher);
        match &request.precondition {
            WritePrecondition::Version(version) => (0_u8, version.as_str()).hash(&mut hasher),
            WritePrecondition::Absent => 1_u8.hash(&mut hasher),
            WritePrecondition::Any => 2_u8.hash(&mut hasher),
        }
        hasher.finish()
    }

    pub(crate) fn contains(&self, operation: &OperationId) -> bool {
        self.entries().done.contains_key(operation)
    }

    /// 已执行过时返回第一次的回执；参数不同返回冲突。
    pub(crate) fn replay(
        &self,
        operation: &OperationId,
        fingerprint: u64,
    ) -> Result<Option<FileWritten>, EnvError> {
        match self.entries().done.get(operation) {
            Some((recorded, written)) if *recorded == fingerprint => Ok(Some(written.clone())),
            Some(_) => Err(EnvError::OperationConflict {
                operation: operation.clone(),
            }),
            None => Ok(None),
        }
    }

    pub(crate) fn record(&self, operation: OperationId, fingerprint: u64, written: FileWritten) {
        let mut entries = self.entries();
        if entries
            .done
            .insert(operation.clone(), (fingerprint, written))
            .is_none()
        {
            entries.order.push_back(operation);
        }
        while entries.order.len() > CAPACITY {
            if let Some(oldest) = entries.order.pop_front() {
                entries.done.remove(&oldest);
            }
        }
    }
}
