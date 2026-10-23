//! MemoryEnvironment 的写入与代码回退事务（docs/specs/rust-cli/session-lifecycle.md §5.2）。
//!
//! 内存文件表在一把锁里核对并写入，所以 apply 的“再核对”与写入是原子的；语义与 env-local 相同：
//! 有一个文件不符就整体失败，不改任何文件。

use std::collections::{BTreeMap, HashMap};

use crate::env::{EnvError, FileVersion, WritePrecondition};
use crate::env_files::{FileWritten, PutFile};
use crate::env_rewind::{
    RestoreAction, RestoreFile, RestoredFile, RewindConflict, RewindPlan, RewindStatus,
    content_hash,
};
use crate::ids::OperationId;

pub(crate) fn version(bytes: &[u8]) -> FileVersion {
    FileVersion::new(format!("v{}-{}", bytes.len(), &content_hash(bytes)[..8]))
}

pub(crate) fn put(
    files: &mut BTreeMap<String, Vec<u8>>,
    request: PutFile,
) -> Result<FileWritten, EnvError> {
    let path = request.path.as_str().to_owned();
    let current = files.get(&path);
    let admitted = match (&request.precondition, current) {
        (WritePrecondition::Any, _) | (WritePrecondition::Absent, None) => true,
        (WritePrecondition::Version(expected), Some(bytes)) => &version(bytes) == expected,
        _ => false,
    };
    if !admitted {
        return Err(EnvError::PreconditionFailed { path });
    }
    let created = current.is_none();
    let written = FileWritten {
        version: version(&request.bytes),
        mtime_ms: 0,
        created,
    };
    files.insert(path, request.bytes);
    Ok(written)
}

fn conflicts(files: &BTreeMap<String, Vec<u8>>, restore: &[RestoreFile]) -> Vec<RewindConflict> {
    restore
        .iter()
        .filter_map(|file| {
            let current = files
                .get(file.path.as_str())
                .map(|bytes| content_hash(bytes));
            (current != file.expected).then(|| RewindConflict {
                path: file.path.clone(),
                current,
                message: None,
            })
        })
        .collect()
}

#[derive(Default)]
pub(crate) struct Transactions(HashMap<OperationId, (Vec<RestoreFile>, RewindStatus)>);

impl Transactions {
    pub(crate) fn prepare(
        &mut self,
        files: &BTreeMap<String, Vec<u8>>,
        token: OperationId,
        restore: Vec<RestoreFile>,
    ) -> RewindPlan {
        let found = conflicts(files, &restore);
        if !found.is_empty() {
            return RewindPlan::Conflicts { files: found };
        }
        self.0
            .entry(token)
            .or_insert((restore, RewindStatus::Prepared));
        RewindPlan::Ready
    }

    pub(crate) fn apply(
        &mut self,
        files: &mut BTreeMap<String, Vec<u8>>,
        token: &OperationId,
    ) -> RewindStatus {
        let Some((restore, status)) = self.0.get_mut(token) else {
            return RewindStatus::Unknown;
        };
        if *status != RewindStatus::Prepared {
            return status.clone();
        }
        let found = conflicts(files, restore);
        *status = if found.is_empty() {
            let mut done = Vec::new();
            for file in restore.iter() {
                let path = file.path.as_str().to_owned();
                let action = match &file.restore {
                    Some(bytes) => {
                        files.insert(path, bytes.clone());
                        RestoreAction::Restore
                    }
                    None => {
                        files.remove(&path);
                        RestoreAction::Delete
                    }
                };
                done.push(RestoredFile {
                    path: file.path.clone(),
                    action,
                });
            }
            RewindStatus::Applied { files: done }
        } else {
            RewindStatus::Failed {
                message: "files changed after prepare".to_owned(),
                conflicts: found,
            }
        };
        status.clone()
    }

    pub(crate) fn status(&self, token: &OperationId) -> RewindStatus {
        self.0
            .get(token)
            .map_or(RewindStatus::Unknown, |(_, status)| status.clone())
    }

    pub(crate) fn finish(&mut self, token: &OperationId) {
        self.0.remove(token);
    }
}
