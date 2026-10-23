//! 代码回退的 token 事务（docs/specs/rust-cli/session-lifecycle.md §5.2）。
//!
//! 计划由内核从 checkpoint 算出；执行环境只按文件核对内容 hash 并写入 / 删除。token 是
//! prepare 时的 `OperationContext.operation`：同一个 token 的 apply 重放返回第一次的结果。
//! 执行侧不跨调用持锁：prepare 与 apply 之间文件可能变化，apply 写每个文件前再核对一次。

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::env::EnvPath;

/// 回退核对用的内容 hash：sha256 的小写 hex（与 Node `file-rewind.ts` 的 `hashContent` 相同）。
/// 内核算期望值、执行环境算当前值，都用这一个函数。
pub fn content_hash(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// 一个文件的回退：期望的当前内容与要恢复成的内容。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreFile {
    pub path: EnvPath,
    /// 期望的当前内容的 sha256（小写 hex）；None 表示文件应当不存在。
    pub expected: Option<String>,
    /// 恢复成的内容；None 表示删除。
    pub restore: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindPrepare {
    pub files: Vec<RestoreFile>,
}

/// 当前内容与期望不符的文件。`current` 是当前内容的 sha256，文件不存在时为 None。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindConflict {
    pub path: EnvPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    /// 读文件失败时的原因（这时不知道当前内容）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// prepare 的结果：全部核对通过才登记 token。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "plan", rename_all = "snake_case")]
pub enum RewindPlan {
    Ready,
    Conflicts { files: Vec<RewindConflict> },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RestoreAction {
    Restore,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoredFile {
    pub path: EnvPath,
    pub action: RestoreAction,
}

/// 事务的状态。`Failed` 时已按记录把写过的文件恢复原样。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum RewindStatus {
    Prepared,
    Applying,
    Applied {
        files: Vec<RestoredFile>,
    },
    Failed {
        message: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        conflicts: Vec<RewindConflict>,
    },
    /// 执行环境不认识这个 token（已 finish，或执行环境重启过）。
    Unknown,
}

#[cfg(test)]
#[path = "env_rewind_tests.rs"]
mod tests;
