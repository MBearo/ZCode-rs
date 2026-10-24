//! 代码回退事务的线上类型（kernel-api `env_rewind`）。

use serde::{Deserialize, Serialize};

use crate::Blob;

/// 一个文件的回退（kernel-api `RestoreFile`）：要写回的内容在二进制段。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restore: Option<Blob>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewindConflict {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

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
    pub path: String,
    pub action: RestoreAction,
}

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
    Unknown,
}
