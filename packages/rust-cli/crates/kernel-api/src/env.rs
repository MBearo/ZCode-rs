//! 执行环境端口（docs/specs/rust-cli/README.md §6）。
//!
//! 工作区文件、搜索、进程、Git、环境事实都在执行环境里。本地实现是 `env-local`；上云后换成远端
//! 实现，内核不变。规则：
//! - 类型化操作，分 read 与 mutate，不提供万能 invoke；
//! - 每个 mutate 操作带 [`OperationContext`]，operationId 幂等；
//! - 只传可序列化的 DTO，不传回调、句柄、PID 或本机路径；
//! - 取消是一条消息，发出后继续等终态。

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

pub use crate::env_command::{CommandEvent, OutputStream, StartCommand};
use crate::env_files::{
    DirEntry, FileStat, FileWritten, GlobFiles, GlobMatches, GrepOutcome, GrepSearch, ListDir,
    PathFacts, PutFile, ReadText, StatFile, TextRead,
};
use crate::env_git::{GitStatus, GitStatusRequest};
use crate::env_rewind::{RewindPlan, RewindPrepare, RewindStatus};
use crate::env_shell::{BackgroundInfo, BackgroundStop, BackgroundTask, ShellNotice, ShellState};
use crate::env_web::{FetchUrl, FetchedUrl};
use crate::env_worktree::{
    WorktreeChanges, WorktreeCreate, WorktreeCreated, WorktreeInspect, WorktreeList, WorktreeRemove,
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
    /// 除 cwd 外允许访问的工作目录。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_directories: Vec<EnvPath>,
    /// 执行侧用户的 home；用户级规则文件 `~/.zcode/AGENTS.md` 由它定位。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<EnvPath>,
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

fn not_provided(what: &str) -> EnvError {
    EnvError::Unsupported {
        what: what.to_owned(),
    }
}

/// 执行环境。实现方必须保证：同一个 operationId 只执行一次；连接断开不等于取消；
/// 结果无法确定时返回 [`Outcome::Unknown`]，不重跑。
///
/// 文件与搜索操作（`path_facts` 起）、shell 状态与通知、git 状态是后加的，默认返回 `Unsupported`，
/// 实现方按需覆盖（docs/specs/rust-cli/tools-files.md §3、context.md §12）。
#[async_trait]
pub trait Environment: Send + Sync {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError>;

    async fn read_file(
        &self,
        workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError>;

    async fn write_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError>;

    async fn start_command(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError>;

    /// 从 `cursor` 起读命令输出，直到终态。
    fn watch_command(
        &self,
        operation: OperationId,
        cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>>;

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError>;

    async fn operation_status(&self, operation: OperationId) -> Result<Outcome, EnvError>;

    /// 会话关闭：`forget` 为 true 时释放执行侧为这个会话保存的全部状态。
    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError>;

    /// 解析用户路径所需的事实（read）。
    async fn path_facts(&self, _workspace: &WorkspaceRef) -> Result<PathFacts, EnvError> {
        Err(not_provided("path_facts"))
    }

    /// 元数据（read）；路径不存在时为 `Ok(None)`。
    async fn stat(
        &self,
        _workspace: &WorkspaceRef,
        _request: StatFile,
    ) -> Result<Option<FileStat>, EnvError> {
        Err(not_provided("stat"))
    }

    /// 目录的直接子项（read）。
    async fn list_dir(
        &self,
        _workspace: &WorkspaceRef,
        _request: ListDir,
    ) -> Result<Vec<DirEntry>, EnvError> {
        Err(not_provided("list_dir"))
    }

    /// 按行读文本（read）；文件不存在时返回 [`EnvError::NotFound`]。
    async fn read_text(
        &self,
        _workspace: &WorkspaceRef,
        _request: ReadText,
    ) -> Result<TextRead, EnvError> {
        Err(not_provided("read_text"))
    }

    /// 条件写整份文件（mutate）；前置条件不满足时返回 [`EnvError::PreconditionFailed`]。
    async fn put_file(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: PutFile,
    ) -> Result<FileWritten, EnvError> {
        Err(not_provided("put_file"))
    }

    /// 按文件名模式找文件（read）。
    async fn glob(
        &self,
        _workspace: &WorkspaceRef,
        _request: GlobFiles,
    ) -> Result<GlobMatches, EnvError> {
        Err(not_provided("glob"))
    }

    /// 工作区文件清单（read，docs/specs/rust-cli/tui-input.md §1.2）。
    async fn list_files(
        &self,
        _workspace: &WorkspaceRef,
        _request: crate::env_files::ListFiles,
    ) -> Result<crate::env_files::FileList, EnvError> {
        Err(not_provided("list_files"))
    }

    /// 内容搜索（read）。
    async fn grep(
        &self,
        _workspace: &WorkspaceRef,
        _request: GrepSearch,
    ) -> Result<GrepOutcome, EnvError> {
        Err(not_provided("grep"))
    }

    /// 会话的 shell 状态（read）：下一条 [`crate::env_shell::CwdScope::Session`] 命令的起始目录。
    async fn shell_state(
        &self,
        _workspace: &WorkspaceRef,
        _session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        Err(not_provided("shell_state"))
    }

    /// 会话里后台命令的终态通知：先送达 `seq > after` 且没有 ack 的，再等新的。
    fn shell_notices(
        &self,
        _session: &SessionId,
        _after: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        futures_util::stream::iter([Err(not_provided("shell_notices"))]).boxed()
    }

    /// 内核已持久化 `seq` 及之前的通知（mutate，幂等）。
    async fn ack_shell_notice(&self, _session: &SessionId, _seq: u64) -> Result<(), EnvError> {
        Err(not_provided("ack_shell_notice"))
    }

    /// 后台命令的登记（read，TaskStop 的校验；shell-permissions.md §2.6）；不认识时为 None。
    async fn background_command(
        &self,
        _request: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        Err(not_provided("background_command"))
    }

    /// 在跑的后台命令从开始到现在的毫秒数（read，Shell 详情的 Runtime，tui-panels.md §3.3）；已结束或
    /// 不认识时为 None。
    async fn background_elapsed(&self, _request: BackgroundTask) -> Result<Option<u64>, EnvError> {
        Err(not_provided("background_elapsed"))
    }

    /// 停止后台命令（mutate，TaskStop）：不发终态通知，立即不再算 [`ShellState::background`]。
    async fn stop_background_command(
        &self,
        _context: OperationContext,
        _request: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        Err(not_provided("stop_background_command"))
    }

    /// `cwd` 所在仓库的 git 状态（read，docs/specs/rust-cli/context.md §12）。
    async fn git_status(
        &self,
        _workspace: &WorkspaceRef,
        _request: GitStatusRequest,
    ) -> Result<GitStatus, EnvError> {
        Err(not_provided("git_status"))
    }

    /// 未提交改动（`/diff`，read）。
    async fn git_diff(
        &self,
        _workspace: &WorkspaceRef,
        _request: crate::env_git::GitDiffRequest,
    ) -> Result<crate::env_git::GitDiff, EnvError> {
        Err(not_provided("git_diff"))
    }

    /// 取一个网页（read，WebFetch；docs/specs/rust-cli/tools-more.md §4）：一次 GET，不跟随重定向。
    async fn fetch_url(
        &self,
        _workspace: &WorkspaceRef,
        _request: FetchUrl,
    ) -> Result<FetchedUrl, EnvError> {
        Err(not_provided("fetch_url"))
    }

    /// PDF 的页数（read，`pdfinfo`；docs/specs/rust-cli/tools-files.md §1.5，修复线 F24）。
    async fn pdf_page_count(
        &self,
        _workspace: &WorkspaceRef,
        _request: crate::env_pdf::PdfPageCount,
    ) -> Result<Option<u64>, EnvError> {
        Err(not_provided("pdf_page_count"))
    }

    /// 把 PDF 的几页渲染成 JPEG（read，`pdftoppm`；tools-files.md §1.5，修复线 F24）。
    async fn render_pdf_pages(
        &self,
        _workspace: &WorkspaceRef,
        _request: crate::env_pdf::RenderPdfPages,
    ) -> Result<crate::env_pdf::PdfRender, EnvError> {
        Err(not_provided("render_pdf_pages"))
    }

    /// 代码回退事务（session-lifecycle.md §5.2）：核对全部文件，通过时以 `context.operation` 登记 token。
    async fn rewind_prepare(
        &self,
        _workspace: &WorkspaceRef,
        _context: OperationContext,
        _request: RewindPrepare,
    ) -> Result<RewindPlan, EnvError> {
        Err(not_provided("rewind_prepare"))
    }

    /// 执行登记过的事务（mutate，按 token 幂等）；失败时已补偿。
    async fn rewind_apply(&self, _token: &OperationId) -> Result<RewindStatus, EnvError> {
        Err(not_provided("rewind_apply"))
    }

    async fn rewind_status(&self, _token: &OperationId) -> Result<RewindStatus, EnvError> {
        Err(not_provided("rewind_status"))
    }

    /// 释放 token 的记录（幂等）。
    async fn rewind_finish(&self, _token: &OperationId) -> Result<(), EnvError> {
        Err(not_provided("rewind_finish"))
    }

    /// 新建或续用 worktree（mutate，docs/specs/rust-cli/tools-more.md §11.2）。
    async fn worktree_create(
        &self,
        _context: OperationContext,
        _request: WorktreeCreate,
    ) -> Result<WorktreeCreated, EnvError> {
        Err(not_provided("worktree_create"))
    }

    /// `cwd` 所在仓库的主仓库根与 `git worktree list`（read）。
    async fn worktree_list(&self, _cwd: EnvPath) -> Result<WorktreeList, EnvError> {
        Err(not_provided("worktree_list"))
    }

    /// worktree 里未保存的工作；查不到时 None（read）。
    async fn worktree_changes(
        &self,
        _request: WorktreeInspect,
    ) -> Result<Option<WorktreeChanges>, EnvError> {
        Err(not_provided("worktree_changes"))
    }

    /// 删除 worktree 与分支（mutate）；返回是否删掉了目录。
    async fn worktree_remove(
        &self,
        _context: OperationContext,
        _request: WorktreeRemove,
    ) -> Result<bool, EnvError> {
        Err(not_provided("worktree_remove"))
    }

    /// 把会话的 shell cwd 设为 `cwd`（mutate）：会话换了工作目录（§11.4）。
    async fn set_shell_cwd(&self, _session: &SessionId, _cwd: EnvPath) -> Result<(), EnvError> {
        Err(not_provided("set_shell_cwd"))
    }
}
