//! 执行环境的公共类型（docs/specs/rust-cli/README.md §6）。
//!
//! 工作区文件、搜索、进程、Git、环境事实都在执行环境里。端口在 [`crate::machine`]（一台机器上的 8 个小端口与
//! [`crate::machine::MachineEnv`]）与 [`crate::controller_services`]（只在本机的 Git 与取网页），机器经
//! [`crate::env_registry`] 取到（remote-exec.md §15.8；R0 前是一个 `Environment` trait）。本地实现是 `env-local`；
//! 上云后换成远端实现，内核不变。这里是端口共用的 DTO：路径、工作区、操作上下文、终态、文件读写与错误。

use std::sync::Arc;

use serde::{Deserialize, Serialize};

use crate::clock::Ids;
pub use crate::env_command::{
    BackgroundReason, CommandEvent, CommandKind, OutputStream, StartCommand,
};
use crate::ids::{EnvironmentId, Generation, OperationId, OwnerEpoch, RunId, SessionId};

/// 执行环境里的路径。与本机存储路径（`rust_cli_paths::StoragePath`）是不同的类型。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct EnvPath(String);

impl EnvPath {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 一个会话所在的工作区：哪个执行环境、根目录在哪。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub environment: EnvironmentId,
    pub root: EnvPath,
}

/// 文件版本：执行侧返回，用于条件写与“读后被改过”的判断。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FileVersion(String);

impl FileVersion {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// 谁发起了这次操作。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Origin {
    Agent { session: SessionId, run: RunId },
    User { session: Option<SessionId> },
}

/// 每次有副作用的操作都带的上下文（§6 规则 3）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationContext {
    pub environment: EnvironmentId,
    pub generation: Generation,
    pub owner_epoch: OwnerEpoch,
    pub operation: OperationId,
    pub origin: Origin,
}

impl OperationContext {
    /// 本机 owner（[`OwnerEpoch::LOCAL`]）的一次操作；换 owner 的一侧用结构更新填 `owner_epoch`。
    pub fn new(
        environment: EnvironmentId,
        generation: Generation,
        operation: OperationId,
        origin: Origin,
    ) -> Self {
        Self {
            environment,
            generation,
            owner_epoch: OwnerEpoch::LOCAL,
            operation,
            origin,
        }
    }
}

/// 本机 owner 的操作上下文的工厂（工作线 F134，modularity.md §7 F131-3，审查 C9）：执行环境的当前实例（generation）
/// 与操作 ID 的来源在一起。端口结构（主循环、压缩、hooks、会话 actor、agents）只带它，不再各自带一个 `generation`、
/// 各自拼五个字段。
#[derive(Clone)]
pub struct Operations {
    generation: Generation,
    ids: Arc<dyn Ids>,
}

impl Operations {
    /// `ids` 与端口结构里取别的 ID 的是同一个（ID 的顺序不变）。
    pub fn new(generation: Generation, ids: Arc<dyn Ids>) -> Self {
        Self { generation, ids }
    }

    /// 一次新的操作：取一个新的操作 ID。
    pub fn next(&self, environment: EnvironmentId, origin: Origin) -> OperationContext {
        self.with_id(environment, self.ids.operation(), origin)
    }

    /// 用已经取好的操作 ID（同一个 ID 在别处也用，例如压缩后重读文件的调用 ID）。
    pub fn with_id(
        &self,
        environment: EnvironmentId,
        operation: OperationId,
        origin: Origin,
    ) -> OperationContext {
        OperationContext::new(environment, self.generation.clone(), operation, origin)
    }
}

/// 操作终态（§6 规则 5）。Unknown 与 Interrupted 不重跑。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Outcome {
    Completed,
    Failed {
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

/// 环境事实：提示词的环境段从这里取，不从内核所在进程取（§6 规则 12）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvFacts {
    pub cwd: EnvPath,
    pub platform: String,
    pub os_version: String,
    pub shell: String,
    pub is_git_repo: bool,
    /// 执行侧用户的 home；用户级规则文件 `~/.zcode/AGENTS.md` 由它定位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<EnvPath>,
    /// 执行侧用户的 uid（POSIX；Windows 与不报告的老执行器没有）。root 不能用 bypassPermissions 的检查看它：命令在
    /// 执行侧、以这个用户跑（deployment.md §21）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub uid: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadFile {
    pub path: EnvPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileContent {
    pub bytes: Vec<u8>,
    pub version: FileVersion,
}

/// 条件写：`precondition` 决定什么情况下允许写入；执行侧在一次操作里完成检查与替换。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteFile {
    pub path: EnvPath,
    pub bytes: Vec<u8>,
    pub precondition: WritePrecondition,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "version", rename_all = "snake_case")]
pub enum WritePrecondition {
    /// 文件当前版本必须是这个。
    Version(FileVersion),
    /// 文件必须不存在。
    Absent,
    /// 不检查。
    Any,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WriteReceipt {
    pub version: FileVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnvError {
    #[error("not found: {path}")]
    NotFound { path: String },
    #[error("precondition failed: {path}")]
    PreconditionFailed { path: String },
    #[error("permission denied: {message}")]
    PermissionDenied { message: String },
    #[error("stale operation context: {message}")]
    Stale { message: String },
    #[error("operation {operation} reused with different arguments")]
    OperationConflict { operation: OperationId },
    #[error("io error: {message}")]
    Io { message: String },
    #[error("not implemented: {what}")]
    Unsupported { what: String },
}
