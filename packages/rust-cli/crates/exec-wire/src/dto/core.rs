//! 各端口共用的线上类型：工作区、操作上下文、终态、环境事实、路径事实。

use serde::{Deserialize, Serialize};

/// 一个会话所在的工作区（kernel-api `WorkspaceRef`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Workspace {
    pub environment: String,
    pub root: String,
}

/// 谁发起了这次操作（kernel-api `Origin`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Agent {
        session: String,
        run: String,
    },
    User {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        session: Option<String>,
    },
}

/// 有副作用的操作带的上下文（kernel-api `OperationContext`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationContext {
    pub environment: String,
    pub generation: String,
    pub owner_epoch: u64,
    pub operation: String,
    pub origin: Origin,
}

/// 操作终态（kernel-api `Outcome`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Failed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        message: String,
    },
    Rejected {
        message: String,
    },
    Running,
    Unknown,
    Interrupted,
    Cancelled,
}

/// 环境事实（kernel-api `EnvFacts`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvFacts {
    pub cwd: String,
    pub platform: String,
    pub os_version: String,
    pub shell: String,
    pub is_git_repo: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
}

/// 路径风格（kernel-api `PathStyle`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PathStyle {
    Posix,
    Windows,
}

/// 解析用户路径所需的事实（kernel-api `PathFacts`）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathFacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
    pub style: PathStyle,
}
