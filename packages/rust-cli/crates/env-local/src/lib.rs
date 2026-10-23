//! 执行环境端口的本地实现（docs/specs/rust-cli/README.md §6）。
//!
//! 工作区文件（版本回执、条件写、apply_patch）、搜索（ignore + grep-searcher，rg 语义）、进程（shell、
//! 后台任务账本、sandbox）、Git、环境事实、规则文件、MCP 传输、插件安装、回退事务。本机文件经
//! 这里访问；进程经 `rust-cli-proc`；网络经 `rust-cli-net`。
//!
//! 已实现：文件与搜索（工作线 L3，docs/specs/rust-cli/tools-files.md §3）；shell 命令、后台通知与
//! sandbox（工作线 L4，docs/specs/rust-cli/shell-permissions.md §2）；环境事实与 git 状态（工作线 L2，
//! docs/specs/rust-cli/context.md §12）；MCP 传输（工作线 L9，docs/specs/rust-cli/ext.md §3.2）。插件目录经
//! 文件操作读取（ext.md §4），安装未覆盖。

mod cron_file;
mod cron_lock;
mod cron_store;
mod cron_watch;
mod files;
mod git;
mod hook_http;
mod host;
mod ledger;
mod mcp;
mod meta;
mod pdf;
mod rewind;
mod search;
mod shell;
mod text;
mod web;

#[path = "lib_builders.rs"]
mod builders;
#[path = "lib_helpers.rs"]
mod helpers;
mod worktree;

use std::path::PathBuf;

use async_trait::async_trait;
use futures_util::stream::{self, BoxStream, StreamExt};
use rust_cli_kernel_api::env::{
    CommandEvent, EnvError, EnvFacts, EnvPath, Environment, FileContent, OperationContext, Outcome,
    ReadFile, StartCommand, WorkspaceRef, WriteFile, WriteReceipt,
};
use rust_cli_kernel_api::env_files::{
    DirEntry, FileStat, FileWritten, GlobFiles, GlobMatches, GrepOutcome, GrepSearch, ListDir,
    PathFacts, PathStyle, PutFile, ReadText, StatFile, TextRead,
};
use rust_cli_kernel_api::env_git::{GitStatus, GitStatusRequest};
use rust_cli_kernel_api::env_rewind::{RewindPlan, RewindPrepare, RewindStatus};
use rust_cli_kernel_api::env_shell::{
    BackgroundInfo, BackgroundStop, BackgroundTask, ShellNotice, ShellState,
};
use rust_cli_kernel_api::env_worktree::{
    WorktreeChanges, WorktreeCreate, WorktreeCreated, WorktreeInspect, WorktreeList, WorktreeRemove,
};
use rust_cli_kernel_api::ids::{OperationId, SessionId};

use crate::helpers::{on_path, path, run, run_walk, unknown_operation, unsupported};
use crate::ledger::Ledger;
use crate::shell::{Shells, environ};

pub use crate::cron_store::LocalCronStore;
pub use crate::hook_http::LocalHookHttp;
pub use crate::mcp::LocalMcpTransport;
pub use crate::shell::{EmbeddedSearch, ShellConfig};

/// 本机执行环境。`home` 由入口 crate 传入（只有它读进程环境），用于 `~` 展开。
pub struct LocalEnvironment {
    home: Option<PathBuf>,
    writes: Ledger,
    /// 代码回退事务（session-lifecycle.md §5.2）。
    rewinds: rewind::Rewinds,
    /// shell 命令；没有配置时命令类操作返回 `Unsupported`。
    shells: Option<Shells>,
    /// 入口 crate 捕获的进程环境（随 shell 配置给出）：环境事实的 `$SHELL`、git 与 uname 的环境。
    snapshot: rust_cli_base::secret::EnvSnapshot,
    /// Bash 工具能用（Windows 上要有 Git Bash）；决定环境事实里的 Shell。
    bash: bool,
    /// 系统版本只取一次（进程内不变）。
    os_version: tokio::sync::OnceCell<String>,
    /// WebFetch 的网络出口；没有配置时 `fetch_url` 返回 `Unsupported`。
    web: Option<std::sync::Arc<rust_cli_net::Egress>>,
    /// pdftoppm 是否可用（可用的结论只查一次，tools-files.md §1.5）。
    poppler: pdf::Poppler,
    /// `--add-dir` 的目录（入口已解析成绝对路径并检查过，cli-args.md §6）：环境事实的附加工作目录。
    additional: Vec<EnvPath>,
}

#[async_trait]
impl Environment for LocalEnvironment {
    /// 环境事实（context.md §12）：cwd 是工作区根，git 仓库判断看最近的含 `.git` 的祖先目录。
    async fn facts(&self, workspace: &WorkspaceRef) -> Result<EnvFacts, EnvError> {
        let root = path(&workspace.root);
        let is_git_repo = run(move || Ok(host::git_root(&root).is_some())).await?;
        let os_version = self
            .os_version
            .get_or_init(|| host::os_version(&self.snapshot))
            .await
            .clone();
        Ok(EnvFacts {
            cwd: workspace.root.clone(),
            platform: host::platform().to_owned(),
            os_version,
            shell: host::describe_shell(environ::shell_variable(&self.snapshot), self.bash),
            is_git_repo,
            additional_directories: self.additional.clone(),
            home: self
                .home
                .as_ref()
                .map(|home| EnvPath::new(home.to_string_lossy())),
        })
    }

    async fn read_file(
        &self,
        _workspace: &WorkspaceRef,
        request: ReadFile,
    ) -> Result<FileContent, EnvError> {
        run(on_path(&request.path, files::read_file)).await
    }

    async fn write_file(
        &self,
        workspace: &WorkspaceRef,
        context: OperationContext,
        request: WriteFile,
    ) -> Result<WriteReceipt, EnvError> {
        let put = PutFile {
            path: request.path,
            bytes: request.bytes,
            precondition: request.precondition,
            create_parents: false,
        };
        let written = self.put_file(workspace, context, put).await?;
        Ok(WriteReceipt {
            version: written.version,
        })
    }

    async fn start_command(
        &self,
        _workspace: &WorkspaceRef,
        context: OperationContext,
        request: StartCommand,
    ) -> Result<(), EnvError> {
        self.shells()?.start(context, request).await
    }

    fn watch_command(
        &self,
        operation: OperationId,
        cursor: u64,
    ) -> BoxStream<'static, Result<CommandEvent, EnvError>> {
        let watched = self.shells().and_then(|shells| {
            shells
                .watch(&operation, cursor)
                .ok_or_else(|| unknown_operation(&operation))
        });
        match watched {
            Ok(events) => events,
            Err(error) => stream::iter([Err(error)]).boxed(),
        }
    }

    async fn cancel(&self, operation: OperationId) -> Result<(), EnvError> {
        if self.shells()?.cancel(&operation) {
            Ok(())
        } else {
            Err(unknown_operation(&operation))
        }
    }

    async fn operation_status(&self, operation: OperationId) -> Result<Outcome, EnvError> {
        if let Some(outcome) = self
            .shells
            .as_ref()
            .and_then(|shells| shells.status(&operation))
        {
            return Ok(outcome);
        }
        Ok(if self.writes.contains(&operation) {
            Outcome::Completed
        } else {
            Outcome::Unknown
        })
    }

    async fn close_session(&self, session: &SessionId, forget: bool) -> Result<(), EnvError> {
        if let Some(shells) = &self.shells {
            shells.close_session(session, forget);
        }
        Ok(())
    }

    async fn path_facts(&self, _workspace: &WorkspaceRef) -> Result<PathFacts, EnvError> {
        Ok(PathFacts {
            home: self
                .home
                .as_ref()
                .map(|home| EnvPath::new(home.to_string_lossy())),
            style: if cfg!(windows) {
                PathStyle::Windows
            } else {
                PathStyle::Posix
            },
        })
    }

    async fn stat(
        &self,
        _workspace: &WorkspaceRef,
        request: StatFile,
    ) -> Result<Option<FileStat>, EnvError> {
        run(on_path(&request.path, files::stat)).await
    }

    async fn list_dir(
        &self,
        _workspace: &WorkspaceRef,
        request: ListDir,
    ) -> Result<Vec<DirEntry>, EnvError> {
        run(on_path(&request.path, files::list_dir)).await
    }

    async fn read_text(
        &self,
        _workspace: &WorkspaceRef,
        request: ReadText,
    ) -> Result<TextRead, EnvError> {
        let target = path(&request.path);
        run(move || files::read_text(&target, &request)).await
    }

    async fn put_file(
        &self,
        _workspace: &WorkspaceRef,
        context: OperationContext,
        request: PutFile,
    ) -> Result<FileWritten, EnvError> {
        // 同一个 operationId 只执行一次（spec §6 规则 3）：重放返回第一次的回执，参数不同则拒绝。
        let fingerprint = Ledger::fingerprint(&request);
        if let Some(done) = self.writes.replay(&context.operation, fingerprint)? {
            return Ok(done);
        }
        let target = path(&request.path);
        let written = run(move || files::put_file(&target, &request)).await?;
        self.writes
            .record(context.operation, fingerprint, written.clone());
        Ok(written)
    }

    async fn glob(
        &self,
        _workspace: &WorkspaceRef,
        request: GlobFiles,
    ) -> Result<GlobMatches, EnvError> {
        run_walk(move |stop| search::glob(&request, stop)).await
    }

    async fn grep(
        &self,
        _workspace: &WorkspaceRef,
        request: GrepSearch,
    ) -> Result<GrepOutcome, EnvError> {
        run_walk(move |stop| search::grep(&request, stop)).await
    }

    async fn list_files(
        &self,
        _workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env_files::ListFiles,
    ) -> Result<rust_cli_kernel_api::env_files::FileList, EnvError> {
        run_walk(move |stop| search::list_files(&request, stop)).await
    }

    async fn shell_state(
        &self,
        workspace: &WorkspaceRef,
        session: &SessionId,
    ) -> Result<ShellState, EnvError> {
        Ok(self.shells()?.shell_state(workspace, session))
    }

    fn shell_notices(
        &self,
        session: &SessionId,
        after: u64,
    ) -> BoxStream<'static, Result<ShellNotice, EnvError>> {
        match self.shells() {
            Ok(shells) => shells.notices(session, after),
            Err(error) => stream::iter([Err(error)]).boxed(),
        }
    }

    async fn ack_shell_notice(&self, session: &SessionId, seq: u64) -> Result<(), EnvError> {
        self.shells()?.ack(session, seq);
        Ok(())
    }

    async fn background_command(
        &self,
        request: BackgroundTask,
    ) -> Result<Option<BackgroundInfo>, EnvError> {
        Ok(self.shells()?.background_command(&request))
    }

    async fn stop_background_command(
        &self,
        context: OperationContext,
        request: BackgroundTask,
    ) -> Result<BackgroundStop, EnvError> {
        self.shells()?.stop(&context, &request)
    }

    async fn background_elapsed(&self, request: BackgroundTask) -> Result<Option<u64>, EnvError> {
        Ok(self.shells()?.background_elapsed(&request))
    }

    async fn git_status(
        &self,
        _workspace: &WorkspaceRef,
        request: GitStatusRequest,
    ) -> Result<GitStatus, EnvError> {
        git::status(&path(&request.cwd), self.git_env()).await
    }

    async fn git_diff(
        &self,
        _workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env_git::GitDiffRequest,
    ) -> Result<rust_cli_kernel_api::env_git::GitDiff, EnvError> {
        git::diff(&path(&request.cwd), self.git_env()).await
    }

    async fn fetch_url(
        &self,
        _workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env_web::FetchUrl,
    ) -> Result<rust_cli_kernel_api::env_web::FetchedUrl, EnvError> {
        let egress = self.web.as_ref().ok_or_else(|| unsupported("web fetch"))?;
        web::fetch(egress, request).await
    }

    /// PDF 的页数（`pdfinfo -`，tools-files.md §1.5）：环境是白名单投影（同 git）。
    async fn pdf_page_count(
        &self,
        _workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env_pdf::PdfPageCount,
    ) -> Result<Option<u64>, EnvError> {
        pdf::page_count(&path(&request.path), &self.git_env()).await
    }

    /// PDF 取页（`pdftoppm`）：输出目录在 shell 输出所在的私有临时目录下，所以要有 shell 配置。
    async fn render_pdf_pages(
        &self,
        _workspace: &WorkspaceRef,
        request: rust_cli_kernel_api::env_pdf::RenderPdfPages,
    ) -> Result<rust_cli_kernel_api::env_pdf::PdfRender, EnvError> {
        let scratch = self.shells()?.scratch().to_path_buf();
        let env = self.git_env();
        let source = path(&request.path);
        self.poppler.render(&source, &request, &scratch, &env).await
    }

    async fn rewind_prepare(
        &self,
        _workspace: &WorkspaceRef,
        context: OperationContext,
        request: RewindPrepare,
    ) -> Result<RewindPlan, EnvError> {
        let files = request.files.clone();
        let found = run(move || Ok(rewind::conflicts(&files))).await?;
        if !found.is_empty() {
            return Ok(RewindPlan::Conflicts { files: found });
        }
        self.rewinds.register(context.operation, request.files);
        Ok(RewindPlan::Ready)
    }

    async fn rewind_apply(&self, token: &OperationId) -> Result<RewindStatus, EnvError> {
        let files = match self.rewinds.begin(token) {
            Ok(files) => files,
            Err(status) => return Ok(status),
        };
        // 阻塞线程本身失败时也要落一个终态，不让 token 停在 Applying。
        let status = run(move || Ok(rewind::apply(&files)))
            .await
            .unwrap_or_else(|error| RewindStatus::Failed {
                message: error.to_string(),
                conflicts: Vec::new(),
            });
        self.rewinds.end(token, status.clone());
        Ok(status)
    }

    async fn rewind_status(&self, token: &OperationId) -> Result<RewindStatus, EnvError> {
        Ok(self.rewinds.status(token))
    }

    async fn rewind_finish(&self, token: &OperationId) -> Result<(), EnvError> {
        self.rewinds.finish(token);
        Ok(())
    }

    async fn worktree_create(
        &self,
        _context: OperationContext,
        request: WorktreeCreate,
    ) -> Result<WorktreeCreated, EnvError> {
        worktree::create(request, self.git_env()).await
    }

    async fn worktree_list(&self, cwd: EnvPath) -> Result<WorktreeList, EnvError> {
        worktree::list(&cwd, self.git_env()).await
    }

    async fn worktree_changes(
        &self,
        request: WorktreeInspect,
    ) -> Result<Option<WorktreeChanges>, EnvError> {
        worktree::changes(request, self.git_env()).await
    }

    async fn worktree_remove(
        &self,
        _context: OperationContext,
        request: WorktreeRemove,
    ) -> Result<bool, EnvError> {
        worktree::remove(request, self.git_env()).await
    }

    async fn set_shell_cwd(&self, session: &SessionId, cwd: EnvPath) -> Result<(), EnvError> {
        self.shells()?.set_cwd(session, path(&cwd));
        Ok(())
    }
}

#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
