//! 代码回退的 token 事务（docs/specs/rust-cli/session-lifecycle.md §5.2）。
//!
//! prepare 核对每个文件的内容 hash，全部通过才登记 token；apply 对每个文件写之前再核对一次
//! （prepare 与 apply 之间不持锁，文件可能又被改），先记下当前内容再写 / 删，任一步失败就按
//! 记录逆序补偿。登记表的锁只保护内存操作，文件 IO 在锁外的阻塞线程里做。

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use rust_cli_kernel_api::env::{EnvError, WritePrecondition};
use rust_cli_kernel_api::env_files::PutFile;
use rust_cli_kernel_api::env_rewind::{
    RestoreAction, RestoreFile, RestoredFile, RewindConflict, RewindStatus, content_hash,
};
use rust_cli_kernel_api::ids::OperationId;

use crate::files;
use crate::meta::{display, io_error};

#[derive(Default)]
pub(crate) struct Rewinds(Mutex<HashMap<OperationId, (Vec<RestoreFile>, RewindStatus)>>);

impl Rewinds {
    fn table(&self) -> MutexGuard<'_, HashMap<OperationId, (Vec<RestoreFile>, RewindStatus)>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// 登记（同一个 token 再次 prepare 时保留第一次的记录）。
    pub(crate) fn register(&self, token: OperationId, files: Vec<RestoreFile>) {
        self.table()
            .entry(token)
            .or_insert((files, RewindStatus::Prepared));
    }

    /// 取出要执行的文件并标为 Applying；不是 Prepared 时返回已有的状态。
    pub(crate) fn begin(&self, token: &OperationId) -> Result<Vec<RestoreFile>, RewindStatus> {
        let mut table = self.table();
        let Some((files, status)) = table.get_mut(token) else {
            return Err(RewindStatus::Unknown);
        };
        if *status != RewindStatus::Prepared {
            return Err(status.clone());
        }
        *status = RewindStatus::Applying;
        Ok(files.clone())
    }

    pub(crate) fn end(&self, token: &OperationId, result: RewindStatus) {
        if let Some((_, status)) = self.table().get_mut(token) {
            *status = result;
        }
    }

    pub(crate) fn status(&self, token: &OperationId) -> RewindStatus {
        self.table()
            .get(token)
            .map_or(RewindStatus::Unknown, |(_, status)| status.clone())
    }

    pub(crate) fn finish(&self, token: &OperationId) {
        self.table().remove(token);
    }
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）"
)]
fn current(path: &Path) -> Result<Option<Vec<u8>>, EnvError> {
    match fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(io_error(path, &error)),
    }
}

/// 当前内容与期望不符的文件。
pub(crate) fn conflicts(files: &[RestoreFile]) -> Vec<RewindConflict> {
    files
        .iter()
        .filter_map(|file| {
            let path = PathBuf::from(file.path.as_str());
            match current(&path) {
                Ok(bytes) => {
                    let hash = bytes.as_deref().map(content_hash);
                    (hash != file.expected).then(|| RewindConflict {
                        path: file.path.clone(),
                        current: hash,
                        message: None,
                    })
                }
                Err(error) => Some(RewindConflict {
                    path: file.path.clone(),
                    current: None,
                    message: Some(error.to_string()),
                }),
            }
        })
        .collect()
}

#[expect(
    clippy::disallowed_methods,
    reason = "env-local 是工作区文件 IO 的入口（docs/specs/rust-cli/README.md §4）"
)]
fn set(path: &Path, bytes: Option<&[u8]>) -> Result<(), EnvError> {
    match bytes {
        Some(bytes) => {
            let request = PutFile {
                path: rust_cli_kernel_api::env::EnvPath::new(display(path)),
                bytes: bytes.to_vec(),
                precondition: WritePrecondition::Any,
                create_parents: true,
            };
            files::put_file(path, &request).map(drop)
        }
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(io_error(path, &error)),
        },
    }
}

/// 执行：逐个再核对 → 记下当前内容 → 写 / 删；失败时逆序补偿，返回 `Failed`。
pub(crate) fn apply(files: &[RestoreFile]) -> RewindStatus {
    let mut journal: Vec<(PathBuf, Option<Vec<u8>>)> = Vec::new();
    let mut done = Vec::new();
    let mut failure: Option<(String, Vec<RewindConflict>)> = None;
    for file in files {
        let path = PathBuf::from(file.path.as_str());
        let found = conflicts(std::slice::from_ref(file));
        if !found.is_empty() {
            failure = Some((
                "files changed after the rewind was prepared".to_owned(),
                found,
            ));
            break;
        }
        let before = match current(&path) {
            Ok(before) => before,
            Err(error) => {
                failure = Some((error.to_string(), Vec::new()));
                break;
            }
        };
        journal.push((path.clone(), before));
        if let Err(error) = set(&path, file.restore.as_deref()) {
            failure = Some((error.to_string(), Vec::new()));
            break;
        }
        done.push(RestoredFile {
            path: file.path.clone(),
            action: if file.restore.is_some() {
                RestoreAction::Restore
            } else {
                RestoreAction::Delete
            },
        });
    }
    let Some((message, conflicts)) = failure else {
        return RewindStatus::Applied { files: done };
    };
    for (path, before) in journal.into_iter().rev() {
        if let Err(error) = set(&path, before.as_deref()) {
            // 补偿失败：工作区处于半回退状态，原因一并报告。
            return RewindStatus::Failed {
                message: format!("{message}; restoring {} failed: {error}", display(&path)),
                conflicts,
            };
        }
    }
    RewindStatus::Failed { message, conflicts }
}
