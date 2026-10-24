//! 一台机器上的执行端口（docs/specs/rust-cli/remote-exec.md §15.8、§15.8.1；README §6）。
//!
//! 工作区文件、搜索、进程、shell 状态、后台命令、代码回退、PDF 与 MCP 的管道都在执行它的那台机器上。本机的实现
//! 是 `env-local`；远端机器（R1 起）另有实现，内核不变。R0 前这些方法都在一个 `Environment` trait 里（36 个），
//! 现在按能力拆成 8 个小端口，[`MachineEnv`] 把它们与 MCP 传输装成一台机器。只在本机的 Git 与取网页在
//! [`crate::local_services`]。
//!
//! 规则（与拆分前相同）：
//! - 类型化操作，分 read 与 mutate，不提供万能 invoke；
//! - 每个 mutate 操作带 [`OperationContext`]，operationId 幂等；
//! - 只传可序列化的 DTO，不传回调、句柄、PID 或本机路径；
//! - 取消是一条消息，发出后继续等终态。
//!
//! 实现方必须保证：同一个 operationId 只执行一次；连接断开不等于取消；结果无法确定时返回
//! [`crate::env::Outcome::Unknown`]，不重跑。后加的方法默认返回 `Unsupported`，实现方按需覆盖
//! （docs/specs/rust-cli/tools-files.md §3、context.md §12）。

use std::sync::Arc;

use async_trait::async_trait;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;

use crate::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, FileContent, OperationContext, Outcome, ReadFile,
    StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use crate::env_files::{
    DirEntry, FileList, FileStat, FileWritten, GlobFiles, GlobMatches, GrepOutcome, GrepSearch,
    ListDir, ListFiles, PathFacts, PutFile, ReadText, StatFile, TextRead,
};
use crate::env_pdf::{PdfPageCount, PdfRender, RenderPdfPages};
use crate::env_rewind::{RewindPlan, RewindPrepare, RewindStatus};
use crate::env_shell::{BackgroundInfo, BackgroundStop, BackgroundTask, ShellNotice, ShellState};
use crate::ids::{OperationId, SessionId};
use crate::mcp::McpTransport;

pub(crate) fn not_provided(what: &str) -> EnvError {
    EnvError::Unsupported {
        what: what.to_owned(),
    }
}

/// 环境事实（提示词的环境段、路径解析，§6 规则 12）。
#[async_trait]
pub trait EnvInfo: Send + Sync {
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError>;

    /// 解析用户路径所需的事实（read）。
    async fn path_facts(&self, _workspace: &WorkspaceRef) -> Result<PathFacts, EnvError> {
        Err(not_provided("path_facts"))
    }
}

/// 工作区文件的读写与元数据。
#[async_trait]
pub trait Files: Send + Sync {
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
}

/// 按文件名与内容搜索。
#[async_trait]
pub trait Search: Send + Sync {
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
        _request: ListFiles,
    ) -> Result<FileList, EnvError> {
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
}

/// 跑命令：启动、按游标读输出、取消、查终态，会话关闭时释放。
#[async_trait]
pub trait Commands: Send + Sync {
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
}

/// 会话的 shell 状态与后台命令的终态通知。
#[async_trait]
pub trait Shell: Send + Sync {
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

    /// 把会话的 shell cwd 设为 `cwd`（mutate）：会话换了工作目录（tools-more.md §11.4）。
    async fn set_shell_cwd(&self, _session: &SessionId, _cwd: EnvPath) -> Result<(), EnvError> {
        Err(not_provided("set_shell_cwd"))
    }
}

/// 后台命令的登记、运行时长与停止。
#[async_trait]
pub trait Background: Send + Sync {
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
}

/// 代码回退事务（session-lifecycle.md §5.2）。
#[async_trait]
pub trait Checkpoints: Send + Sync {
    /// 核对全部文件，通过时以 `context.operation` 登记 token。
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
}

/// PDF 的页数与取页（在执行它的机器上渲染，remote-exec.md §15.7）。
#[async_trait]
pub trait Pdf: Send + Sync {
    /// PDF 的页数（read，`pdfinfo`；docs/specs/rust-cli/tools-files.md §1.5，修复线 F24）。
    async fn pdf_page_count(
        &self,
        _workspace: &WorkspaceRef,
        _request: PdfPageCount,
    ) -> Result<Option<u64>, EnvError> {
        Err(not_provided("pdf_page_count"))
    }

    /// 把 PDF 的几页渲染成 JPEG（read，`pdftoppm`；tools-files.md §1.5，修复线 F24）。
    async fn render_pdf_pages(
        &self,
        _workspace: &WorkspaceRef,
        _request: RenderPdfPages,
    ) -> Result<PdfRender, EnvError> {
        Err(not_provided("render_pdf_pages"))
    }
}

/// 一台机器：8 个执行端口与 MCP 的管道（remote-exec.md §15.8.1）。
///
/// 只经 [`MachineEnv::new`] 建：8 个端口总是同一个对象（一台机器不会一半在本机、一半在远端）。拿到其中一个端口的
/// 代码只看得到这个端口的方法（端口 trait 不继承 `std::any::Any`，不能向下转型）。
#[derive(Clone)]
pub struct MachineEnv {
    info: Arc<dyn EnvInfo>,
    files: Arc<dyn Files>,
    search: Arc<dyn Search>,
    commands: Arc<dyn Commands>,
    shell: Arc<dyn Shell>,
    background: Arc<dyn Background>,
    checkpoints: Arc<dyn Checkpoints>,
    pdf: Arc<dyn Pdf>,
    mcp: Arc<dyn McpTransport>,
}

impl MachineEnv {
    /// `env` 实现全部 8 个端口（本机是 env-local 的 `LocalEnvironment`）；`mcp` 是这台机器上的 MCP 传输（本机是
    /// `LocalMcpTransport`）。远端一条连接两者都实现。
    pub fn new<E>(env: Arc<E>, mcp: Arc<dyn McpTransport>) -> Self
    where
        E: EnvInfo + Files + Search + Commands + Shell + Background + Checkpoints + Pdf + 'static,
    {
        Self {
            info: env.clone(),
            files: env.clone(),
            search: env.clone(),
            commands: env.clone(),
            shell: env.clone(),
            background: env.clone(),
            checkpoints: env.clone(),
            pdf: env,
            mcp,
        }
    }

    pub fn info(&self) -> &Arc<dyn EnvInfo> {
        &self.info
    }

    pub fn files(&self) -> &Arc<dyn Files> {
        &self.files
    }

    pub fn search(&self) -> &Arc<dyn Search> {
        &self.search
    }

    pub fn commands(&self) -> &Arc<dyn Commands> {
        &self.commands
    }

    pub fn shell(&self) -> &Arc<dyn Shell> {
        &self.shell
    }

    pub fn background(&self) -> &Arc<dyn Background> {
        &self.background
    }

    pub fn checkpoints(&self) -> &Arc<dyn Checkpoints> {
        &self.checkpoints
    }

    pub fn pdf(&self) -> &Arc<dyn Pdf> {
        &self.pdf
    }

    pub fn mcp(&self) -> &Arc<dyn McpTransport> {
        &self.mcp
    }
}
